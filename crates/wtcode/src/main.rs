//! wtcode: 위에는 레포의 git 워크트리 목록(Enter·클릭으로 VS Code), 아래에는 오늘 투두 파일(todo.rs).
//! 연 뒤에도 꺼지지 않는다 — tmux pane 에 띄워 두고 쓴다. 워크트리와 투두 파일이 바뀌면 화면이 따라 바뀐다.

mod render;
mod target;
mod todo;

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{bail, Context, Result};
use ratatui::{
    crossterm::{
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
        execute,
    },
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};

const LIST_REFRESH: Duration = Duration::from_secs(5);
const DIRTY_REFRESH: Duration = Duration::from_secs(10);

struct Worktree {
    path: PathBuf,
    name: String,
    branch: String,
    issue: Option<u32>,
    /// `git status --porcelain` 줄 수. 아직 세는 중이면 None.
    dirty: Option<usize>,
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `repo-gh123` → 123. 폴더 이름에 없으면 브랜치 이름의 `gh123`·`GH-123` 에서 찾는다.
fn issue_number(name: &str, branch: &str) -> Option<u32> {
    let digits_after = |s: &str, pat: &str| -> Option<u32> {
        let lower = s.to_ascii_lowercase();
        let i = lower.rfind(pat)? + pat.len();
        lower[i..].trim_start_matches('-').chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
    };
    digits_after(name, "-gh").or_else(|| digits_after(branch, "gh"))
}

/// `git worktree list --porcelain` 순서(메인 먼저, 그다음 만든 순서). 폴더가 없는 워크트리는 뺀다.
fn list_worktrees(repo: &Path) -> Vec<Worktree> {
    let Some(raw) = git(repo, &["worktree", "list", "--porcelain"]) else { return Vec::new() };
    raw.split("\n\n")
        .filter_map(|block| {
            let mut path = None;
            let mut branch = String::new();
            for l in block.lines() {
                if let Some(p) = l.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(p));
                } else if let Some(b) = l.strip_prefix("branch ") {
                    branch = b.trim_start_matches("refs/heads/").to_string();
                } else if l == "detached" {
                    branch = "(detached)".into();
                }
            }
            let path = path.filter(|p| p.is_dir())?;
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let issue = issue_number(&name, &branch);
            Some(Worktree { path, name, branch, issue, dirty: None })
        })
        .collect()
}

/// 미커밋 수는 워크트리가 많으면 느려 뒤에서 센다.
fn spawn_dirty(paths: Vec<PathBuf>, tx: Sender<(PathBuf, usize)>) {
    thread::spawn(move || {
        for p in paths {
            let n = git(&p, &["status", "--porcelain"]).map(|s| s.lines().count()).unwrap_or(0);
            if tx.send((p, n)).is_err() {
                return;
            }
        }
    });
}

/// WTCODE_EDITOR 로 바꿀 수 있다: {path} 가 채워진다. 기본은 `code {path}`.
fn open_in_editor(path: &Path) -> String {
    let tpl = env::var("WTCODE_EDITOR").unwrap_or_else(|_| "code {path}".into());
    let parts: Vec<String> = tpl.split_whitespace().map(|t| t.replace("{path}", &path.to_string_lossy())).collect();
    let Some((cmd, args)) = parts.split_first() else { return "WTCODE_EDITOR 가 비었다".into() };
    match Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        // 기다리지 않으면 좀비로 남는다 — 따로 거둔다.
        Ok(mut child) => {
            thread::spawn(move || child.wait());
            format!("열었다: {}", path.display())
        }
        Err(e) => format!("{cmd} 실행 실패: {e}"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Worktrees,
    Todo,
}

/// 투두 칸의 입력 상태.
enum Mode {
    Normal,
    /// 새 투두 입력 중.
    Adding(String),
    /// 지울지 묻는 중(줄 번호, 그 줄).
    Deleting(usize, String),
    /// 보낼 Claude 세션 고르는 중(목록, 고른 칸, 고른 뒤 보낼 것: 글, Enter 까지 칠지).
    Picking(Vec<target::ClaudePane>, usize, Option<(String, bool)>),
}

struct Todo {
    path: PathBuf,
    lines: Vec<String>,
    mtime: Option<SystemTime>,
    /// 고른 체크박스 줄(파일 줄 번호).
    sel: Option<usize>,
    /// 화면 줄 단위 스크롤.
    scroll: usize,
    area: Rect,
    /// 마지막으로 그린 화면 줄마다 파일 줄 번호(클릭용).
    rows_src: Vec<Option<usize>>,
    /// 다음 그리기에서 고른 항목이 보이게 스크롤할지 — 키로 고를 때만. 휠로 굴린 위치는 그대로 둔다.
    follow: bool,
}

impl Todo {
    fn tasks(&self) -> Vec<usize> {
        (0..self.lines.len()).filter(|&i| todo::task(&self.lines[i]).is_some()).collect()
    }

    /// 파일이 바뀌었으면 다시 읽는다. 고른 줄은 같은 내용의 줄로 옮긴다.
    fn reload(&mut self, force: bool) {
        let m = fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if !force && m == self.mtime {
            return;
        }
        let keep = self.sel.and_then(|i| self.lines.get(i).cloned());
        self.lines = fs::read_to_string(&self.path).unwrap_or_default().lines().map(String::from).collect();
        self.mtime = m;
        let tasks = self.tasks();
        self.sel = keep
            .and_then(|k| tasks.iter().copied().find(|&i| self.lines[i] == k))
            .or_else(|| self.sel.and_then(|s| tasks.iter().copied().find(|&i| i >= s)))
            .or_else(|| tasks.first().copied());
    }

    fn move_by(&mut self, d: isize) {
        let tasks = self.tasks();
        if tasks.is_empty() {
            return;
        }
        let cur = self.sel.and_then(|s| tasks.iter().position(|&i| i == s)).unwrap_or(0) as isize;
        // 첫 투두에서 더 위로 가면 맨 위(끝난 것·진행 중)를 보인다 — 투두가 아닌 곳은 고를 수 없어서.
        if d < 0 && cur == 0 {
            self.scroll = 0;
            self.follow = false;
            return;
        }
        self.sel = Some(tasks[(cur + d).clamp(0, tasks.len() as isize - 1) as usize]);
        self.follow = true;
    }

    /// 보이는 범위만 굴린다(선택은 그대로). 끝은 그릴 때 맞춘다.
    fn scroll_by(&mut self, d: isize) {
        self.scroll = (self.scroll as isize + d).max(0) as usize;
        self.follow = false;
    }

    fn page(&self) -> isize {
        self.area.height.saturating_sub(3).max(1) as isize
    }
}

struct App {
    repo: PathBuf,
    items: Vec<Worktree>,
    list: ListState,
    list_area: Rect,
    todo: Todo,
    focus: Focus,
    mode: Mode,
    notice: Option<(String, Instant)>,
    /// 투두를 보낼 Claude pane.
    target: Option<target::ClaudePane>,
    listed_at: Instant,
    dirty_at: Instant,
    tx: Sender<(PathBuf, usize)>,
    rx: Receiver<(PathBuf, usize)>,
}

impl App {
    /// 목록을 다시 읽는다. 고른 워크트리는 경로로 유지하고, 이미 센 미커밋 수는 옮겨 둔다.
    fn reload(&mut self, recount: bool) {
        let keep = self.selected().map(|w| w.path.clone());
        let mut fresh = list_worktrees(&self.repo);
        for w in &mut fresh {
            w.dirty = self.items.iter().find(|o| o.path == w.path).and_then(|o| o.dirty);
        }
        let need: Vec<PathBuf> = fresh.iter().filter(|w| recount || w.dirty.is_none()).map(|w| w.path.clone()).collect();
        self.items = fresh;
        let at = keep.and_then(|p| self.items.iter().position(|w| w.path == p)).or((!self.items.is_empty()).then_some(0));
        self.list.select(at);
        self.listed_at = Instant::now();
        if recount {
            self.dirty_at = Instant::now();
        }
        if !need.is_empty() {
            spawn_dirty(need, self.tx.clone());
        }
    }

    fn selected(&self) -> Option<&Worktree> {
        self.list.selected().and_then(|i| self.items.get(i))
    }

    fn move_by(&mut self, d: isize) {
        if self.items.is_empty() {
            return;
        }
        let cur = self.list.selected().unwrap_or(0) as isize;
        self.list.select(Some((cur + d).clamp(0, self.items.len() as isize - 1) as usize));
    }

    fn open_selected(&mut self) {
        if let Some(p) = self.selected().map(|w| w.path.clone()) {
            self.say(open_in_editor(&p));
        }
    }

    fn say(&mut self, m: impl Into<String>) {
        self.notice = Some((m.into(), Instant::now()));
    }

    /// 투두 파일을 고친 결과를 알리고 다시 읽는다.
    fn after_edit(&mut self, r: Result<(), String>) {
        if let Err(e) = r {
            self.say(e);
        }
        self.todo.reload(true);
    }

    fn toggle_todo(&mut self) {
        let Some(at) = self.todo.sel else { return };
        let expect = self.todo.lines[at].clone();
        let r = todo::toggle(&self.todo.path, at, &expect);
        self.after_edit(r);
    }

    fn poll(&mut self) {
        while let Ok((p, n)) = self.rx.try_recv() {
            if let Some(w) = self.items.iter_mut().find(|w| w.path == p) {
                w.dirty = Some(n);
            }
        }
        if self.dirty_at.elapsed() >= DIRTY_REFRESH {
            self.reload(true);
        } else if self.listed_at.elapsed() >= LIST_REFRESH {
            self.reload(false);
        }
        // 날이 바뀌면 새 파일로 옮겨 간다.
        if self.todo.path.file_stem().is_none_or(|s| s.to_string_lossy() != todo::today()) {
            if let Ok(p) = todo::ensure_today() {
                self.todo.path = p;
                self.todo.sel = None;
            }
        }
        self.todo.reload(false);
    }

    /// true 를 돌려주면 끝낸다.
    fn on_key(&mut self, k: KeyEvent) -> bool {
        // 입력·확인 중에는 키를 그쪽에서 받는다.
        match &mut self.mode {
            Mode::Adding(buf) => {
                match k.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Enter => {
                        let text = std::mem::take(buf);
                        self.mode = Mode::Normal;
                        if !text.trim().is_empty() {
                            let r = todo::add(&self.todo.path, &text);
                            self.after_edit(r);
                            self.todo.sel = self.todo.tasks().into_iter().rfind(|&i| self.todo.lines[i].ends_with(text.trim()));
                        }
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) => buf.push(c),
                    _ => {}
                }
                return false;
            }
            Mode::Deleting(at, expect) => {
                let (at, expect) = (*at, expect.clone());
                self.mode = Mode::Normal;
                if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    let r = todo::delete(&self.todo.path, at, &expect);
                    self.after_edit(r);
                }
                return false;
            }
            Mode::Picking(list, at, pending) => {
                match k.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Char('j') | KeyCode::Down => *at = (*at + 1).min(list.len().saturating_sub(1)),
                    KeyCode::Char('k') | KeyCode::Up => *at = at.saturating_sub(1),
                    KeyCode::Enter => {
                        let (pick, pending) = (list.get(*at).cloned(), pending.take());
                        self.mode = Mode::Normal;
                        self.pick(pick, pending);
                    }
                    _ => {}
                }
                return false;
            }
            Mode::Normal => {}
        }
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Worktrees { Focus::Todo } else { Focus::Worktrees };
            }
            KeyCode::Char('t') => self.open_picker(None),
            KeyCode::Char('a') => {
                self.focus = Focus::Todo;
                self.mode = Mode::Adding(String::new());
            }
            KeyCode::Char('r') => {
                self.reload(true);
                self.todo.reload(true);
            }
            _ if self.focus == Focus::Todo => match k.code {
                KeyCode::Char('p') => self.send_selected(false),
                KeyCode::Char('P') => self.send_selected(true),
                KeyCode::Char('y') => {
                    if let Some(text) = self.selected_text() {
                        let r = target::copy(&text).map(|_| format!("복사했다: {text}"));
                        self.say(r.unwrap_or_else(|e| e));
                    }
                }
                KeyCode::Char('j') | KeyCode::Down => self.todo.move_by(1),
                KeyCode::Char('k') | KeyCode::Up => self.todo.move_by(-1),
                KeyCode::PageDown => self.todo.scroll_by(self.todo.page()),
                KeyCode::PageUp => self.todo.scroll_by(-self.todo.page()),
                KeyCode::Char('g') | KeyCode::Home => self.todo.scroll_by(isize::MIN / 2),
                KeyCode::Char('G') | KeyCode::End => self.todo.scroll_by(isize::MAX / 2),
                KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Char('x') => self.toggle_todo(),
                KeyCode::Char('d') => {
                    if let Some(at) = self.todo.sel {
                        self.mode = Mode::Deleting(at, self.todo.lines[at].clone());
                    }
                }
                _ => {}
            },
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('g') | KeyCode::Home => self.move_by(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.move_by(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Char('o') => self.open_selected(),
            _ => {}
        }
        false
    }

    /// 고른 투두의 글(체크박스 표시를 뺀 것).
    fn selected_text(&self) -> Option<String> {
        let l = self.todo.lines.get(self.todo.sel?)?;
        todo::task(l)?;
        Some(l.trim_start()[5..].trim().to_string())
    }

    fn open_picker(&mut self, pending: Option<(String, bool)>) {
        let list = target::claude_panes();
        if list.is_empty() {
            self.say("Claude 가 도는 tmux pane 을 찾지 못했다");
            return;
        }
        let at = self.target.as_ref().and_then(|t| list.iter().position(|p| p.id == t.id)).unwrap_or(0);
        self.mode = Mode::Picking(list, at, pending);
    }

    fn pick(&mut self, pick: Option<target::ClaudePane>, pending: Option<(String, bool)>) {
        let Some(p) = pick else { return };
        self.say(format!("보낼 세션: {}", p.label));
        self.target = Some(p);
        if let Some((text, submit)) = pending {
            self.send(&text, submit);
        }
    }

    fn send(&mut self, text: &str, submit: bool) {
        let Some(t) = self.target.clone().filter(|t| target::alive(&t.id)) else {
            // 대상이 없거나 사라졌으면 고른 뒤 보낸다.
            self.open_picker(Some((text.to_string(), submit)));
            return;
        };
        let r = target::send(&t.id, text, submit).map(|_| format!("{} {}: {text}", t.label, if submit { "← 보냄" } else { "← 넣음" }));
        self.say(r.unwrap_or_else(|e| e));
    }

    fn send_selected(&mut self, submit: bool) {
        if let Some(text) = self.selected_text() {
            self.send(&text, submit);
        }
    }

    fn on_click(&mut self, col: u16, row: u16) {
        // 고르기 목록이 떠 있으면 누른 줄을 고른다.
        if let Mode::Picking(list, _, pending) = &mut self.mode {
            let t = self.todo.area;
            let i = row.saturating_sub(t.y + 1) as usize;
            if row > t.y && i < list.len() {
                let (pick, pending) = (list.get(i).cloned(), pending.take());
                self.mode = Mode::Normal;
                self.pick(pick, pending);
            }
            return;
        }
        let a = self.list_area;
        if row > a.y && row < a.y + a.height.saturating_sub(1) && col >= a.x && col < a.x + a.width {
            self.focus = Focus::Worktrees;
            let i = self.list.offset() + (row - a.y - 1) as usize;
            if i < self.items.len() {
                self.list.select(Some(i));
                self.open_selected();
            }
            return;
        }
        let t = self.todo.area;
        if row > t.y && row < t.y + t.height.saturating_sub(1) && col >= t.x && col < t.x + t.width {
            self.focus = Focus::Todo;
            let r = self.todo.scroll + (row - t.y - 1) as usize;
            let Some(Some(at)) = self.todo.rows_src.get(r).copied() else { return };
            if self.todo.lines.get(at).is_some_and(|l| todo::task(l).is_some()) {
                self.todo.sel = Some(at);
                // 그 항목의 첫 화면 줄에서 체크박스(줄 앞 몇 칸)를 누르면 바로 체크한다.
                let first = r == 0 || self.todo.rows_src[r - 1] != Some(at);
                let indent = self.todo.lines[at].len() - self.todo.lines[at].trim_start().len();
                if first && (col - t.x) as usize <= indent + 3 {
                    self.toggle_todo();
                }
            }
        }
    }
}

fn border(focused: bool) -> Style {
    if focused { Style::default().fg(Color::Cyan) } else { Style::default().fg(Color::DarkGray) }
}

fn draw_worktrees(f: &mut Frame, app: &mut App, area: Rect) {
    let name_w = app.items.iter().map(|w| w.name.chars().count()).max().unwrap_or(0).min(40);
    let gray = Style::default().fg(Color::DarkGray);
    let items: Vec<ListItem> = app
        .items
        .iter()
        .map(|w| {
            let issue = w.issue.map(|n| format!("gh{n}")).unwrap_or_default();
            let dirty = match w.dirty {
                None => Span::styled("…   ", gray),
                Some(0) => Span::styled("    ", gray),
                Some(n) => Span::styled(format!("●{n:<3}"), Style::default().fg(Color::Yellow)),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<name_w$}  ", w.name), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(format!("{issue:<7}"), Style::default().fg(Color::Cyan)),
                dirty,
                Span::raw(" "),
                Span::styled(w.branch.clone(), gray),
            ]))
        })
        .collect();
    // 제목은 메인 워크트리(목록 첫 줄) 이름 — 어느 워크트리에서 띄웠든 레포 이름이 보이게.
    let repo = app.items.first().map(|w| w.name.as_str()).unwrap_or_default();
    let title = format!(" {repo} · 워크트리 {} ", app.items.len());
    let focused = app.focus == Focus::Worktrees;
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(border(focused)).title(title))
        .highlight_style(if focused { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default().add_modifier(Modifier::BOLD) })
        .highlight_symbol("▶ ");
    app.list_area = area;
    f.render_stateful_widget(list, area, &mut app.list);
}

fn draw_todo(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &mut app.todo;
    let focused = app.focus == Focus::Todo;
    let mut rows = render::render(&t.lines, area.width.saturating_sub(2) as usize);
    if focused {
        for r in rows.iter_mut().filter(|r| r.src.is_some() && r.src == t.sel) {
            r.line = std::mem::take(&mut r.line).style(Style::default().add_modifier(Modifier::REVERSED));
        }
    }
    // 고른 항목의 화면 줄이 모두 보이게 스크롤한다.
    let h = area.height.saturating_sub(2) as usize;
    let span = t.sel.map(|s| {
        let first = rows.iter().position(|r| r.src == Some(s)).unwrap_or(0);
        let last = rows.iter().rposition(|r| r.src == Some(s)).unwrap_or(first);
        (first, last)
    });
    if let Some((first, last)) = span.filter(|_| t.follow) {
        if first < t.scroll {
            t.scroll = first;
        } else if h > 0 && last >= t.scroll + h {
            t.scroll = (last + 1).saturating_sub(h).min(first);
        }
    }
    t.follow = false;
    t.scroll = t.scroll.min(rows.len().saturating_sub(h));
    t.rows_src = rows.iter().map(|r| r.src).collect();
    let to = app.target.as_ref().map(|p| format!(" → {}", p.label)).unwrap_or_default();
    let title = format!(" 투두 · {}{to} ", t.path.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default());
    let p = Paragraph::new(rows.into_iter().map(|r| r.line).collect::<Vec<_>>())
        .block(Block::default().borders(Borders::ALL).border_style(border(focused)).title(title))
        .scroll((t.scroll as u16, 0));
    t.area = area;
    f.render_widget(p, area);
}

fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    // 워크트리 칸은 워크트리 수만큼(최대 40%), 나머지는 투두.
    let wt_h = (app.items.len() as u16 + 2).min(main.height * 2 / 5).max(3);
    let [top, bottom] = Layout::vertical([Constraint::Length(wt_h), Constraint::Min(3)]).areas(main);
    draw_worktrees(f, app, top);
    draw_todo(f, app, bottom);
    if let Mode::Picking(list, at, _) = &app.mode {
        // 투두 칸 자리에 고르기 목록을 띄운다.
        let items: Vec<ListItem> = list.iter().map(|p| ListItem::new(p.label.clone())).collect();
        let mut st = ListState::default().with_selected(Some(*at));
        f.render_widget(ratatui::widgets::Clear, bottom);
        f.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Yellow)).title(" 투두를 보낼 Claude 세션 "))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("▶ "),
            bottom,
            &mut st,
        );
    }
    let gray = Style::default().fg(Color::DarkGray);
    let text = match &app.mode {
        Mode::Adding(buf) => Line::from(vec![Span::styled(" 새 투두: ", Style::default().fg(Color::Yellow)), Span::raw(format!("{buf}▌")), Span::styled("  Enter 추가 · Esc 취소", gray)]),
        Mode::Picking(..) => Line::from(Span::styled(" ↑↓·클릭 고르기 · Enter 정하기 · Esc 닫기", gray)),
        Mode::Deleting(at, _) => Line::from(Span::styled(
            format!(" 지울까? y/n — {}", app.todo.lines.get(*at).map(|l| l.trim()).unwrap_or("")),
            Style::default().fg(Color::Yellow),
        )),
        Mode::Normal => match &app.notice {
            Some((m, at)) if at.elapsed() < Duration::from_secs(4) => Line::from(Span::styled(format!(" {m}"), gray)),
            _ if app.focus == Focus::Todo => Line::from(Span::styled(" space 체크 · p 세션에 넣기 · P 보내기 · y 복사 · t 세션 고르기 · a 추가 · d 지우기 · Tab", gray)),
            _ => Line::from(Span::styled(" ↑↓ 이동 · Enter·클릭 VS Code · Tab 투두 · a 투두 추가 · r 새로고침 · q 종료", gray)),
        },
    };
    f.render_widget(Paragraph::new(text), status);
}

fn run(term: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        app.poll();
        term.draw(|f| draw(f, app))?;
        if !event::poll(Duration::from_millis(300))? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                if app.on_key(k) {
                    return Ok(());
                }
            }
            Event::Mouse(m) => match m.kind {
                // 휠은 마우스가 올라간 칸을 굴린다. 투두 칸은 선택을 옮기지 않고 보이는 범위만.
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                    let d = if m.kind == MouseEventKind::ScrollDown { 1 } else { -1 };
                    let t = app.todo.area;
                    if m.row >= t.y && m.row < t.y + t.height {
                        app.todo.scroll_by(3 * d);
                    } else {
                        app.move_by(d);
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => app.on_click(m.column, m.row),
                _ => {}
            },
            _ => {}
        }
    }
}

fn usage() -> &'static str {
    "사용: wtcode [레포 안 경로]   워크트리 목록(Enter·클릭 VS Code)과 오늘 투두\n\
     \x20     wtcode todo           오늘 투두 파일 경로를 출력한다(없으면 어제 파일에서 진행 중·남은 투두를 옮겨 만든다)\n\
     WTCODE_EDITOR 로 여는 명령을 바꾼다({path}, 기본 `code {path}`). WTCODE_TODO_DIR 로 투두 폴더를 바꾼다."
}

fn main() -> Result<()> {
    let arg = env::args().nth(1);
    match arg.as_deref() {
        Some("-h" | "--help") => {
            println!("{}", usage());
            return Ok(());
        }
        // 에이전트가 오늘 파일을 찾을 때 쓴다.
        Some("todo") => {
            println!("{}", todo::ensure_today().context("투두 파일을 만들지 못했다")?.display());
            return Ok(());
        }
        _ => {}
    }
    let dir = match arg {
        Some(a) => PathBuf::from(a),
        None => env::current_dir()?,
    };
    let top = git(&dir, &["rev-parse", "--show-toplevel"]).context("git 레포 안에서 실행하거나 레포 경로를 넘긴다")?;
    let repo = PathBuf::from(top.trim());
    let todo_path = todo::ensure_today().context("투두 파일을 만들지 못했다")?;
    let (tx, rx) = mpsc::channel();
    let mut app = App {
        repo,
        items: Vec::new(),
        list: ListState::default(),
        list_area: Rect::default(),
        todo: Todo {
            path: todo_path,
            lines: Vec::new(),
            mtime: None,
            sel: None,
            scroll: 0,
            area: Rect::default(),
            rows_src: Vec::new(),
            // 처음에는 맨 위(끝난 것·진행 중)부터 보인다.
            follow: false,
        },
        focus: Focus::Worktrees,
        mode: Mode::Normal,
        notice: None,
        target: None,
        listed_at: Instant::now(),
        dirty_at: Instant::now(),
        tx,
        rx,
    };
    app.reload(true);
    app.todo.reload(true);
    app.target = target::default_target(&target::claude_panes());
    if app.items.is_empty() {
        bail!("워크트리를 읽지 못했다: {}", app.repo.display());
    }
    let mut term = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut term, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_from_name_or_branch() {
        assert_eq!(issue_number("myrepo-gh123", "feature/x"), Some(123));
        assert_eq!(issue_number("myrepo", "feat/GH-45-login"), Some(45));
        assert_eq!(issue_number("myrepo", "develop"), None);
    }
}
