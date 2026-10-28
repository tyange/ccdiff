use std::{
    collections::HashMap,
    env,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use ratatui::{
    crossterm::{
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind},
        execute,
    },
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};
use serde_json::Value;
use syntect::{
    easy::HighlightLines,
    highlighting::Theme,
    parsing::{SyntaxReference, SyntaxSet},
};
use two_face::theme::EmbeddedThemeName;

const MAX_CREATE_LINES: usize = 3000;

struct Hunk {
    old_start: u64,
    new_start: u64,
    lines: Vec<String>,
}

struct EditEntry {
    time: String,
    kind: &'static str,
    path: PathBuf,
    root: Option<PathBuf>,
    hunks: Vec<Hunk>,
    added: usize,
    removed: usize,
    user_modified: bool,
}

impl EditEntry {
    fn short_path(&self) -> String {
        match &self.root {
            Some(root) => {
                let name = root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                let rel = self.path.strip_prefix(root).unwrap_or(&self.path);
                format!("{} · {}", name, rel.display())
            }
            None => self.path.display().to_string(),
        }
    }

    /// 목록용 짧은 워크트리 이름: `abc-web-apps-gh2171` → `gh2171`, 메인 레포는 `main`.
    fn label(&self) -> String {
        let Some(root) = &self.root else { return "-".into() };
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match name.rfind("-gh") {
            Some(i) => name[i + 1..].to_string(),
            None if name.chars().any(|c| c == '-') && name.rsplit('-').next().is_some_and(|s| s.chars().all(|c| c.is_ascii_digit())) => {
                name.rsplit('-').next().unwrap_or_default().to_string()
            }
            None => "main".into(),
        }
    }

    fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// 세션 JSONL 을 이어 읽는다. 쓰는 중인 마지막 줄은 다음 읽기까지 남겨 둔다.
struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    fn read_lines(&mut self) -> Result<Vec<String>> {
        let mut f = File::open(&self.path)?;
        let len = f.metadata()?.len();
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        f.seek(SeekFrom::Start(self.offset))?;
        f.take(len - self.offset).read_to_end(&mut self.partial)?;
        self.offset = len;

        let mut lines = Vec::new();
        let mut start = 0;
        for (i, b) in self.partial.iter().enumerate() {
            if *b == b'\n' {
                lines.push(String::from_utf8_lossy(&self.partial[start..i]).into_owned());
                start = i + 1;
            }
        }
        self.partial.drain(..start);
        Ok(lines)
    }
}

fn git_root(dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

fn local_time(ts: Option<&str>) -> String {
    ts.and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "--".into())
}

fn parse_entry(line: &str, roots: &mut HashMap<PathBuf, Option<PathBuf>>) -> Option<EditEntry> {
    // 기록 대부분은 수정과 무관하다 — JSON 파싱 전에 문자열로 거른다.
    if !line.contains("\"toolUseResult\"") || !line.contains("\"filePath\"") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let r = v.get("toolUseResult")?;
    let path = PathBuf::from(r.get("filePath")?.as_str()?);

    let kind = if r.get("oldString").is_some() {
        "Edit"
    } else if r.get("edits").is_some() {
        "MultiEdit"
    } else {
        match r.get("type").and_then(Value::as_str) {
            Some("create") => "Write+",
            Some("update") => "Write",
            _ => return None,
        }
    };

    let mut hunks: Vec<Hunk> = r
        .get("structuredPatch")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|h| Hunk {
                    old_start: h.get("oldStart").and_then(Value::as_u64).unwrap_or(0),
                    new_start: h.get("newStart").and_then(Value::as_u64).unwrap_or(0),
                    lines: h
                        .get("lines")
                        .and_then(Value::as_array)
                        .map(|ls| ls.iter().filter_map(|l| l.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();

    // 새 파일은 patch 가 비어 있고 전체 내용만 온다.
    if hunks.is_empty() && kind == "Write+" {
        if let Some(content) = r.get("content").and_then(Value::as_str) {
            hunks.push(Hunk {
                old_start: 0,
                new_start: 1,
                lines: content.lines().take(MAX_CREATE_LINES).map(|l| format!("+{l}")).collect(),
            });
        }
    }

    let (mut added, mut removed) = (0, 0);
    for l in hunks.iter().flat_map(|h| &h.lines) {
        match l.chars().next() {
            Some('+') => added += 1,
            Some('-') => removed += 1,
            _ => {}
        }
    }

    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let root = roots.entry(dir.clone()).or_insert_with(|| git_root(&dir)).clone();

    Some(EditEntry {
        time: local_time(v.get("timestamp").and_then(Value::as_str)),
        kind,
        path,
        root,
        hunks,
        added,
        removed,
        user_modified: r.get("userModified").and_then(Value::as_bool).unwrap_or(false),
    })
}

struct App {
    session: String,
    tail: Tail,
    roots: HashMap<PathBuf, Option<PathBuf>>,
    entries: Vec<EditEntry>,
    repo_order: Vec<Option<PathBuf>>,
    filter: Option<usize>,
    list: ListState,
    follow: bool,
    scroll: u16,
    hscroll: u16,
    hide_outside: bool,
    list_area: Rect,
    diff_area: Rect,
    hl: Highlighter,
    rendered: HashMap<usize, Vec<Line<'static>>>,
}

impl App {
    fn ingest(&mut self) -> Result<bool> {
        let mut changed = false;
        for line in self.tail.read_lines()? {
            if let Some(e) = parse_entry(&line, &mut self.roots) {
                if !self.repo_order.contains(&e.root) {
                    self.repo_order.push(e.root.clone());
                }
                self.entries.push(e);
                changed = true;
            }
        }
        if changed && self.follow {
            self.select_last();
        }
        Ok(changed)
    }

    fn visible(&self) -> Vec<usize> {
        let want = self.filter.map(|i| &self.repo_order[i]);
        (0..self.entries.len())
            .filter(|&i| want.is_none_or(|r| &self.entries[i].root == r))
            .filter(|&i| !self.hide_outside || self.entries[i].root.is_some())
            .collect()
    }

    fn select_last(&mut self) {
        let n = self.visible().len();
        self.list.select(n.checked_sub(1));
        self.scroll = 0;
    }

    fn move_by(&mut self, delta: isize) {
        let n = self.visible().len();
        if n == 0 {
            return;
        }
        let cur = self.list.selected().unwrap_or(n - 1) as isize;
        let next = (cur + delta).clamp(0, n as isize - 1) as usize;
        self.list.select(Some(next));
        self.follow = next == n - 1;
        self.scroll = 0;
    }

    fn on_mouse(&mut self, m: &MouseEvent) {
        let pos = Position::new(m.column, m.row);
        let in_list = self.list_area.contains(pos);
        match m.kind {
            MouseEventKind::ScrollDown if in_list => self.move_by(1),
            MouseEventKind::ScrollUp if in_list => self.move_by(-1),
            MouseEventKind::ScrollDown if self.diff_area.contains(pos) => self.scroll = self.scroll.saturating_add(3),
            MouseEventKind::ScrollUp if self.diff_area.contains(pos) => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::Down(_) if in_list && m.row > self.list_area.y => {
                let idx = self.list.offset() + (m.row - self.list_area.y - 1) as usize;
                let n = self.visible().len();
                if idx < n {
                    self.list.select(Some(idx));
                    self.follow = idx == n - 1;
                    self.scroll = 0;
                }
            }
            _ => {}
        }
    }

    fn cycle_filter(&mut self) {
        if self.repo_order.is_empty() {
            return;
        }
        self.filter = match self.filter {
            None => Some(0),
            Some(i) if i + 1 < self.repo_order.len() => Some(i + 1),
            Some(_) => None,
        };
        self.follow = true;
        self.select_last();
    }

    fn filter_label(&self) -> String {
        match self.filter {
            None => "전체".into(),
            Some(i) => match &self.repo_order[i] {
                Some(r) => r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                None => "(git 밖)".into(),
            },
        }
    }
}

fn truncate_left(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width || width < 2 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - (width - 1)).collect();
    format!("…{tail}")
}

struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    fn new() -> Self {
        let theme = two_face::theme::extra().get(EmbeddedThemeName::OneHalfDark).clone();
        Self { syntaxes: two_face::syntax::extra_newlines(), theme }
    }

    fn syntax_for(&self, path: &Path) -> &SyntaxReference {
        path.extension()
            .and_then(|x| x.to_str())
            .and_then(|x| self.syntaxes.find_syntax_by_extension(x))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }
}

fn diff_lines(e: &EditEntry, hl: &Highlighter) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if e.user_modified {
        out.push(Line::from(Span::styled(
            "사용자가 승인 전에 손으로 고친 수정",
            Style::default().fg(Color::Magenta),
        )));
    }
    if e.hunks.is_empty() {
        out.push(Line::from("(diff 없음)"));
    }
    let gutter = Style::default().fg(Color::DarkGray);
    let add_bg = Color::Rgb(22, 52, 34);
    let del_bg = Color::Rgb(66, 28, 30);
    // hunk 는 파일 조각이라 문법 상태가 조각 사이에서 끊긴다 — 조각마다 새로 시작한다.
    for h in &e.hunks {
        let mut hi = HighlightLines::new(hl.syntax_for(&e.path), &hl.theme);
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(
            format!("@@ -{} +{} @@", h.old_start, h.new_start),
            Style::default().fg(Color::Cyan),
        )));
        let (mut old, mut new) = (h.old_start, h.new_start);
        for l in &h.lines {
            let mut chars = l.chars();
            let mark = chars.next().unwrap_or(' ');
            let code = chars.as_str();
            let (o, n, bg, mark_fg) = match mark {
                '+' => {
                    new += 1;
                    (String::new(), (new - 1).to_string(), Some(add_bg), Color::Green)
                }
                '-' => {
                    old += 1;
                    ((old - 1).to_string(), String::new(), Some(del_bg), Color::Red)
                }
                _ => {
                    old += 1;
                    new += 1;
                    ((old - 1).to_string(), (new - 1).to_string(), None, Color::DarkGray)
                }
            };
            let with_bg = |st: Style| match bg {
                Some(c) => st.bg(c),
                None => st,
            };
            let mut spans = vec![
                Span::styled(format!("{o:>5} {n:>5} "), gutter),
                Span::styled(format!("{mark} "), with_bg(Style::default().fg(mark_fg))),
            ];
            let line = format!("{code}\n");
            match hi.highlight_line(&line, &hl.syntaxes) {
                Ok(parts) => spans.extend(parts.into_iter().map(|(st, text)| {
                    let c = st.foreground;
                    Span::styled(
                        text.trim_end_matches('\n').to_string(),
                        with_bg(Style::default().fg(Color::Rgb(c.r, c.g, c.b))),
                    )
                })),
                Err(_) => spans.push(Span::styled(code.to_string(), with_bg(Style::default()))),
            }
            out.push(Line::from(spans));
        }
    }
    out
}

fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    // 좁은 pane(tmux 반쪽 등)에서는 목록을 위, diff 를 아래로 쌓는다.
    let [left, right] = if main.width < 140 {
        Layout::vertical([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(main)
    } else {
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(main)
    };

    let visible = app.visible();
    let name_width = (left.width as usize).saturating_sub(36);
    let items: Vec<ListItem> = visible
        .iter()
        .map(|&i| {
            let e = &app.entries[i];
            let hm = e.time.get(6..11).unwrap_or(&e.time);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{hm} "), Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{:<7}", e.kind)),
                Span::styled(format!("+{:<4}", e.added), Style::default().fg(Color::Green)),
                Span::styled(format!("-{:<4} ", e.removed), Style::default().fg(Color::Red)),
                Span::styled(format!("{:<8} ", e.label()), Style::default().fg(Color::Yellow)),
                Span::raw(truncate_left(&e.file_name(), name_width)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " 수정 {}건 · {} ",
            visible.len(),
            app.filter_label()
        )))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, left, &mut app.list);
    app.list_area = left;
    app.diff_area = right;

    let sel_idx = app.list.selected().and_then(|s| visible.get(s)).copied();
    if let Some(i) = sel_idx {
        if !app.rendered.contains_key(&i) {
            let lines = diff_lines(&app.entries[i], &app.hl);
            app.rendered.insert(i, lines);
        }
    }
    let selected = sel_idx.map(|i| &app.entries[i]);
    let title_width = (right.width as usize).saturating_sub(4);
    let title = match selected {
        Some(e) => {
            let head = format!(" {} · {} · ", e.time, e.label());
            let rel = match &e.root {
                Some(r) => e.path.strip_prefix(r).unwrap_or(&e.path).display().to_string(),
                None => e.path.display().to_string(),
            };
            let room = title_width.saturating_sub(head.chars().count() + 1);
            format!("{head}{} ", truncate_left(&rel, room))
        }
        None => format!(" {} ", app.session),
    };
    let body = sel_idx
        .and_then(|i| app.rendered.get(&i).cloned())
        .unwrap_or_else(|| vec![Line::from("아직 수정이 없다")]);
    let diff = Paragraph::new(body)
        .block(Block::default().borders(Borders::ALL).title(truncate_left(&title, title_width)))
        .scroll((app.scroll, app.hscroll));
    f.render_widget(diff, right);

    let follow = if app.follow { "따라가기 켜짐" } else { "따라가기 꺼짐" };
    f.render_widget(
        Paragraph::new(format!(
            " {follow} │ j/k 이동 · J/K·PgUp/PgDn 스크롤 · ←/→ 가로 · g/G 처음/끝 · f 따라가기 · w 워크트리 · h git 밖 · q 종료"
        ))
        .style(Style::default().fg(Color::DarkGray)),
        status,
    );
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(300))? {
            let ev = event::read()?;
            if let Event::Mouse(m) = &ev {
                app.on_mouse(m);
            }
            if let Event::Key(k) = ev {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if ctrl => return Ok(()),
                    KeyCode::Char('d') if ctrl => app.scroll = app.scroll.saturating_add(20),
                    KeyCode::Char('u') if ctrl => app.scroll = app.scroll.saturating_sub(20),
                    KeyCode::Char('j') | KeyCode::Down => app.move_by(1),
                    KeyCode::Char('k') | KeyCode::Up => app.move_by(-1),
                    KeyCode::Char('J') => app.scroll = app.scroll.saturating_add(1),
                    KeyCode::Char('K') => app.scroll = app.scroll.saturating_sub(1),
                    KeyCode::PageDown => app.scroll = app.scroll.saturating_add(20),
                    KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(20),
                    KeyCode::Right => app.hscroll = app.hscroll.saturating_add(8),
                    KeyCode::Left => app.hscroll = app.hscroll.saturating_sub(8),
                    KeyCode::Char('g') | KeyCode::Home => {
                        app.follow = false;
                        app.move_by(isize::MIN / 2);
                    }
                    KeyCode::Char('G') | KeyCode::End => {
                        app.follow = true;
                        app.select_last();
                    }
                    KeyCode::Char('f') => {
                        app.follow = !app.follow;
                        if app.follow {
                            app.select_last();
                        }
                    }
                    KeyCode::Char('w') => app.cycle_filter(),
                    KeyCode::Char('h') => {
                        app.hide_outside = !app.hide_outside;
                        app.follow = true;
                        app.select_last();
                    }
                    _ => {}
                }
            }
        }
        app.ingest()?;
    }
}

fn projects_dir() -> Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME 이 없다")?;
    Ok(PathBuf::from(home).join(".claude/projects"))
}

/// 인자: 세션 ID 또는 JSONL 경로. 없으면 현재 디렉터리 프로젝트에서 가장 최근 세션.
fn resolve_session(arg: Option<String>) -> Result<PathBuf> {
    let projects = projects_dir()?;
    if let Some(a) = arg {
        let p = PathBuf::from(&a);
        if p.is_file() {
            return Ok(p);
        }
        for dir in fs::read_dir(&projects)?.flatten() {
            let cand = dir.path().join(format!("{a}.jsonl"));
            if cand.is_file() {
                return Ok(cand);
            }
        }
        bail!("세션을 찾지 못했다: {a}");
    }
    // Claude Code 는 cwd 의 영숫자 외 문자를 '-' 로 바꿔 프로젝트 폴더 이름을 만든다.
    let cwd = env::current_dir()?;
    let encoded: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = projects.join(&encoded);
    let newest = fs::read_dir(&dir)
        .with_context(|| format!("프로젝트 기록 폴더가 없다: {}", dir.display()))?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
    newest.map(|e| e.path()).context("세션 기록이 없다")
}

fn main() -> Result<()> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "사용: ccdiff [--list] [세션ID | 세션.jsonl]\n인자가 없으면 현재 디렉터리 프로젝트의 가장 최근 세션을 연다.\n--list 는 화면 없이 수정 목록만 출력한다."
        );
        return Ok(());
    }
    let list_only = args.iter().any(|a| a == "--list");
    args.retain(|a| a != "--list");
    let arg = args.into_iter().next();
    let path = resolve_session(arg)?;
    let session = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();

    let mut app = App {
        session,
        tail: Tail { path, offset: 0, partial: Vec::new() },
        roots: HashMap::new(),
        entries: Vec::new(),
        repo_order: Vec::new(),
        filter: None,
        list: ListState::default(),
        follow: true,
        scroll: 0,
        hscroll: 0,
        hide_outside: true,
        list_area: Rect::default(),
        diff_area: Rect::default(),
        hl: Highlighter::new(),
        rendered: HashMap::new(),
    };
    app.ingest()?;
    app.select_last();

    if list_only {
        for e in app.visible().into_iter().map(|i| &app.entries[i]) {
            println!("{} {:<9} +{:<4} -{:<4} {}", e.time, e.kind, e.added, e.removed, e.short_path());
        }
        return Ok(());
    }

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
