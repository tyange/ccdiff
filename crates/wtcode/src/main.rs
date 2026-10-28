//! wtcode: 레포의 git 워크트리 목록을 보이고, 고르면(Enter·클릭) VS Code 로 연다.
//! 연 뒤에도 꺼지지 않는다 — tmux pane 에 띄워 두고 쓴다. 워크트리가 생기거나 지워지면 목록이 따라 바뀐다.

use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use ratatui::{
    crossterm::{
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
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

struct App {
    repo: PathBuf,
    items: Vec<Worktree>,
    list: ListState,
    list_area: Rect,
    notice: Option<(String, Instant)>,
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
            self.notice = Some((open_in_editor(&p), Instant::now()));
        }
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
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
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
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    app.list_area = main;
    f.render_stateful_widget(list, main, &mut app.list);
    let text = match &app.notice {
        Some((m, at)) if at.elapsed() < Duration::from_secs(4) => format!(" {m}"),
        _ => " ↑↓ 이동 · Enter·클릭 VS Code · r 새로고침 · q 종료".into(),
    };
    f.render_widget(Paragraph::new(text).style(gray), status);
}

fn run(term: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        app.poll();
        term.draw(|f| draw(f, app))?;
        if !event::poll(Duration::from_millis(300))? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                KeyCode::Char('j') | KeyCode::Down => app.move_by(1),
                KeyCode::Char('k') | KeyCode::Up => app.move_by(-1),
                KeyCode::Char('g') | KeyCode::Home => app.move_by(isize::MIN / 2),
                KeyCode::Char('G') | KeyCode::End => app.move_by(isize::MAX / 2),
                KeyCode::Enter | KeyCode::Char('o') => app.open_selected(),
                KeyCode::Char('r') => app.reload(true),
                _ => {}
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollDown => app.move_by(1),
                MouseEventKind::ScrollUp => app.move_by(-1),
                // 테두리 안의 줄을 누르면 고르고 바로 연다.
                MouseEventKind::Down(MouseButton::Left) => {
                    let a = app.list_area;
                    if m.row > a.y && m.row < a.y + a.height.saturating_sub(1) {
                        let i = app.list.offset() + (m.row - a.y - 1) as usize;
                        if i < app.items.len() {
                            app.list.select(Some(i));
                            app.open_selected();
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn main() -> Result<()> {
    let arg = env::args().nth(1);
    if arg.as_deref().is_some_and(|a| a == "-h" || a == "--help") {
        println!(
            "사용: wtcode [레포 안 경로]\n레포의 git 워크트리 목록을 보이고, Enter·클릭으로 VS Code 를 연다.\n\
             WTCODE_EDITOR 로 여는 명령을 바꿀 수 있다({{path}} 가 채워진다, 기본 `code {{path}}`)."
        );
        return Ok(());
    }
    let dir = match arg {
        Some(a) => PathBuf::from(a),
        None => env::current_dir()?,
    };
    let top = git(&dir, &["rev-parse", "--show-toplevel"]).context("git 레포 안에서 실행하거나 레포 경로를 넘긴다")?;
    let repo = PathBuf::from(top.trim());
    let (tx, rx) = mpsc::channel();
    let mut app = App {
        repo,
        items: Vec::new(),
        list: ListState::default(),
        list_area: Rect::default(),
        notice: None,
        listed_at: Instant::now(),
        dirty_at: Instant::now(),
        tx,
        rx,
    };
    app.reload(true);
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
