mod git;
mod highlight;
mod replay;
mod rows;
mod session;
mod ui;
mod worktree;

use std::{
    collections::HashMap,
    env, fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use ratatui::{
    crossterm::{
        event::{
            self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
            MouseEvent, MouseEventKind,
        },
        execute,
    },
    layout::{Position, Rect},
    widgets::ListState,
    DefaultTerminal,
};

use git::{CommitState, GitIndex};
use highlight::Highlighter;
use replay::{Replay, Step};
use rows::{build_rows, Row, RowKind};
use session::{bash_roots, parse_entry, EditEntry, Tail};

/// 작업 트리 항목의 키 시작값. 세션 수정은 0부터, 작업 트리 항목은 WT 부터 센다.
pub const WT: usize = 1 << 40;
const WT_REFRESH: Duration = Duration::from_secs(10);

/// 한 파일의 수정들을 앞뒤로 넘기며 파일 전체로 보는 화면.
pub struct FileView {
    /// 같은 파일을 고친 수정들(항목 키, 시간순). 작업 트리 항목이 있으면 맨 끝.
    pub seq: Vec<usize>,
    pub pos: usize,
    pub scroll: u16,
    /// 다음 그리기에서 첫 변경 줄로 스크롤한다.
    pub jump: bool,
    pub replay: Option<Replay>,
}

pub struct App {
    pub session: String,
    tail: Tail,
    roots: HashMap<PathBuf, Option<PathBuf>>,
    pub entries: Vec<EditEntry>,
    repo_order: Vec<Option<PathBuf>>,
    filter: Option<usize>,
    pub list: ListState,
    pub follow: bool,
    pub scroll: u16,
    pub hscroll: u16,
    hide_outside: bool,
    pub list_area: Rect,
    pub diff_area: Rect,
    pub hl: Highlighter,
    pub rows: HashMap<usize, Vec<Row>>,
    pub file: Option<FileView>,
    git: GitIndex,
    pub commits: HashMap<usize, CommitState>,
    /// 0 전체 · 1 미커밋만 · 2 커밋만
    commit_filter: u8,
    /// git diff 를 볼 워크트리(세션이 고쳤거나 Bash 로 다룬 곳).
    wt_roots: Vec<PathBuf>,
    pub wt: Vec<EditEntry>,
    wt_at: Option<Instant>,
    /// 세션 첫 기록 시각. 그 전부터 있던 작업 트리 변경은 기본으로 숨긴다.
    session_start: i64,
    pub show_old_wt: bool,
}

impl App {
    fn ingest(&mut self) -> Result<bool> {
        let mut changed = false;
        for line in self.tail.read_lines()? {
            if self.session_start == 0 {
                self.session_start = session::line_epoch(&line);
            }
            for r in bash_roots(&line, &mut self.roots) {
                if !self.wt_roots.contains(&r) {
                    self.wt_roots.push(r);
                    self.wt_at = None;
                }
            }
            if let Some(e) = parse_entry(&line, &mut self.roots) {
                if let Some(r) = &e.root {
                    if !self.wt_roots.contains(r) {
                        self.wt_roots.push(r.clone());
                        self.wt_at = None;
                    }
                }
                if !self.repo_order.contains(&e.root) {
                    self.repo_order.push(e.root.clone());
                }
                if let Some(fv) = &self.file {
                    if self.entry(fv.seq[0]).path == e.path {
                        let key = self.entries.len();
                        let fv = self.file.as_mut().expect("file view");
                        let at = if fv.seq.last().is_some_and(|&k| k >= WT) { fv.seq.len() - 1 } else { fv.seq.len() };
                        fv.seq.insert(at, key);
                    }
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

    pub fn entry(&self, key: usize) -> &EditEntry {
        if key >= WT { &self.wt[key - WT] } else { &self.entries[key] }
    }

    pub fn ensure_rows(&mut self, key: usize) {
        if !self.rows.contains_key(&key) {
            let rows = build_rows(self.entry(key), &self.hl);
            self.rows.insert(key, rows);
        }
    }

    /// 작업 트리 항목을 다시 읽는다. 열려 있는 파일 보기의 작업 트리 키도 새 번호로 맞춘다.
    fn refresh_worktrees(&mut self) {
        if self.wt_at.is_some_and(|t| t.elapsed() < WT_REFRESH) {
            return;
        }
        self.wt_at = Some(Instant::now());
        self.wt = self.wt_roots.iter().flat_map(|r| worktree::entries(r)).collect();
        self.rows.retain(|&k, _| k < WT);
        for e in &self.wt {
            if !self.repo_order.contains(&e.root) {
                self.repo_order.push(e.root.clone());
            }
        }
        if let Some(fv) = &self.file {
            let path = self.entry(fv.seq[0]).path.clone();
            let wt_key = self.wt.iter().position(|e| e.path == path).map(|j| WT + j);
            let fv = self.file.as_mut().expect("file view");
            fv.seq.retain(|&k| k < WT);
            fv.seq.extend(wt_key);
            if fv.seq.is_empty() {
                self.file = None;
            } else if fv.pos >= fv.seq.len() {
                fv.pos = fv.seq.len() - 1;
            }
        }
        if self.follow {
            self.select_last();
        }
    }

    pub fn visible(&self) -> Vec<usize> {
        let want = self.filter.map(|i| &self.repo_order[i]);
        let since = if self.show_old_wt { i64::MIN } else { self.session_start - 60 };
        // 지워진 파일은 mtime 이 없어 0 이다 — 시각으로 거르지 않는다.
        let wt = (0..self.wt.len()).filter(|&j| self.wt[j].epoch == 0 || self.wt[j].epoch >= since).map(|j| WT + j);
        let mut keys: Vec<usize> = (0..self.entries.len()).chain(wt).collect();
        keys.sort_by_key(|&k| self.entry(k).epoch);
        keys.into_iter()
            .filter(|&i| want.is_none_or(|r| &self.entry(i).root == r))
            .filter(|&i| !self.hide_outside || self.entry(i).root.is_some())
            .filter(|&i| match self.commit_filter {
                1 => !self.is_committed(i),
                2 => self.is_committed(i),
                _ => true,
            })
            .collect()
    }

    pub fn is_committed(&self, i: usize) -> bool {
        matches!(self.commits.get(&i), Some(CommitState::Yes { .. } | CommitState::Changed { .. } | CommitState::Gone))
    }

    pub fn uncommitted_count(&self) -> usize {
        (0..self.entries.len())
            .filter(|&i| self.entries[i].root.is_some() && matches!(self.commits.get(&i), Some(CommitState::No)))
            .count()
    }

    pub fn commit_filter_label(&self) -> &'static str {
        match self.commit_filter {
            1 => "미커밋만",
            2 => "커밋만",
            _ => "",
        }
    }

    /// 판정이 안 됐거나 아직 미커밋인 수정만 다시 본다 — 커밋됨은 바뀌지 않는다.
    fn refresh_commits(&mut self) {
        let expired = self.git.expire();
        let since = self.entries.iter().map(|e| e.epoch).filter(|&t| t > 0).min().unwrap_or(0) - 60;
        for i in 0..self.entries.len() {
            let known = self.commits.get(&i);
            let recheck = match known {
                None => true,
                Some(CommitState::Yes { .. } | CommitState::Changed { .. }) => false,
                Some(_) => expired,
            };
            if recheck {
                let st = self.git.state(&self.entries[i], since);
                self.commits.insert(i, st);
            }
        }
    }

    fn selected_entry(&self) -> Option<usize> {
        let v = self.visible();
        self.list.selected().and_then(|s| v.get(s)).copied()
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

    pub fn filter_label(&self) -> String {
        match self.filter {
            None => "전체".into(),
            Some(i) => match &self.repo_order[i] {
                Some(r) => r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                None => "(git 밖)".into(),
            },
        }
    }

    fn open_file(&mut self) {
        let Some(idx) = self.selected_entry() else { return };
        let path = &self.entry(idx).path;
        let seq: Vec<usize> = (0..self.entries.len())
            .filter(|&i| &self.entries[i].path == path)
            .chain(self.wt.iter().position(|e| &e.path == path).map(|j| WT + j))
            .collect();
        let pos = seq.iter().position(|&i| i == idx).unwrap_or(0);
        self.hscroll = 0;
        self.file = Some(FileView { seq, pos, scroll: 0, jump: true, replay: None });
    }

    fn file_step(&mut self, delta: isize) {
        if let Some(fv) = &mut self.file {
            let next = (fv.pos as isize + delta).clamp(0, fv.seq.len() as isize - 1) as usize;
            fv.pos = next;
            fv.jump = true;
            fv.replay = None;
        }
    }

    fn start_replay(&mut self) {
        if let Some(fv) = &mut self.file {
            fv.pos = 0;
            fv.replay = Some(Replay::new());
        }
    }

    fn replaying(&self) -> bool {
        self.file.as_ref().and_then(|v| v.replay.as_ref()).is_some_and(|r| !r.paused)
    }

    fn tick_replay(&mut self) {
        let Some(idx) = self.file.as_ref().filter(|v| v.replay.is_some()).map(|v| v.seq[v.pos]) else { return };
        self.ensure_rows(idx);
        let rows = &self.rows[&idx];
        let total: usize = rows.iter().filter(|r| r.kind == RowKind::Add).map(|r| r.width() + 1).sum();
        let has_del = rows.iter().any(|r| r.kind == RowKind::Del);
        let fv = self.file.as_mut().expect("file view");
        let Some(rp) = fv.replay.as_mut() else { return };
        if let Step::Next = rp.tick(total, has_del) {
            if fv.pos + 1 < fv.seq.len() {
                fv.pos += 1;
                rp.restart_edit();
            } else {
                fv.replay = None;
            }
        }
    }

    fn on_mouse(&mut self, m: &MouseEvent) {
        let pos = Position::new(m.column, m.row);
        if let Some(fv) = &mut self.file {
            if fv.replay.is_none() {
                match m.kind {
                    MouseEventKind::ScrollDown => fv.scroll = fv.scroll.saturating_add(3),
                    MouseEventKind::ScrollUp => fv.scroll = fv.scroll.saturating_sub(3),
                    _ => {}
                }
            }
            return;
        }
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

    /// true 를 돌려주면 종료한다.
    fn on_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('c') if ctrl => return true,
            KeyCode::Char('t') => {
                self.hl.next_theme();
                self.rows.clear();
                return false;
            }
            KeyCode::Char('b') => {
                self.hl.light = !self.hl.light;
                self.rows.clear();
                return false;
            }
            KeyCode::Right => {
                self.hscroll = self.hscroll.saturating_add(8);
                return false;
            }
            KeyCode::Left => {
                self.hscroll = self.hscroll.saturating_sub(8);
                return false;
            }
            _ => {}
        }
        if self.file.is_some() {
            self.on_file_key(k, ctrl);
            return false;
        }
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Enter | KeyCode::Char('o') => self.open_file(),
            KeyCode::Char('d') if ctrl => self.scroll = self.scroll.saturating_add(20),
            KeyCode::Char('u') if ctrl => self.scroll = self.scroll.saturating_sub(20),
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('J') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Char('K') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(20),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(20),
            KeyCode::Char('g') | KeyCode::Home => {
                self.follow = false;
                self.move_by(isize::MIN / 2);
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.follow = true;
                self.select_last();
            }
            KeyCode::Char('f') => {
                self.follow = !self.follow;
                if self.follow {
                    self.select_last();
                }
            }
            KeyCode::Char('w') => self.cycle_filter(),
            KeyCode::Char('a') => {
                self.show_old_wt = !self.show_old_wt;
                self.follow = true;
                self.select_last();
            }
            KeyCode::Char('c') => {
                self.commit_filter = (self.commit_filter + 1) % 3;
                self.follow = true;
                self.select_last();
            }
            KeyCode::Char('h') => {
                self.hide_outside = !self.hide_outside;
                self.follow = true;
                self.select_last();
            }
            _ => {}
        }
        false
    }

    fn on_file_key(&mut self, k: KeyEvent, ctrl: bool) {
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                let fv = self.file.as_mut().expect("file view");
                if fv.replay.is_some() {
                    fv.replay = None;
                } else {
                    self.file = None;
                }
                return;
            }
            KeyCode::Char('n') | KeyCode::Char(']') => return self.file_step(1),
            KeyCode::Char('p') | KeyCode::Char('[') => return self.file_step(-1),
            KeyCode::Char('r') => return self.start_replay(),
            KeyCode::Char(' ') => {
                if let Some(rp) = self.file.as_mut().and_then(|v| v.replay.as_mut()) {
                    rp.toggle_pause();
                }
                return;
            }
            _ => {}
        }
        let fv = self.file.as_mut().expect("file view");
        match k.code {
            KeyCode::Char('j') | KeyCode::Char('J') | KeyCode::Down => fv.scroll = fv.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Char('K') | KeyCode::Up => fv.scroll = fv.scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => fv.scroll = fv.scroll.saturating_add(20),
            KeyCode::Char('u') if ctrl => fv.scroll = fv.scroll.saturating_sub(20),
            KeyCode::PageDown => fv.scroll = fv.scroll.saturating_add(20),
            KeyCode::PageUp => fv.scroll = fv.scroll.saturating_sub(20),
            KeyCode::Char('g') | KeyCode::Home => fv.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => fv.scroll = u16::MAX,
            _ => {}
        }
    }
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut last_sync = Instant::now();
    loop {
        if last_sync.elapsed() >= Duration::from_secs(3) {
            last_sync = Instant::now();
            if app.hl.sync_background() {
                app.rows.clear();
            }
        }
        app.tick_replay();
        // git 호출이 화면을 잠깐 멈추게 하므로 재생 중에는 미룬다.
        if !app.replaying() {
            app.refresh_commits();
            app.refresh_worktrees();
        }
        terminal.draw(|f| ui::draw(f, app))?;
        let wait = if app.replaying() { 30 } else { 300 };
        if event::poll(Duration::from_millis(wait))? {
            match event::read()? {
                Event::Mouse(m) => app.on_mouse(&m),
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if app.on_key(k) {
                        return Ok(());
                    }
                }
                _ => {}
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

fn args_verify() -> bool {
    env::args().any(|a| a == "--verify")
}

fn main() -> Result<()> {
    let mut args: Vec<String> = env::args().skip(1).filter(|a| a != "--verify").collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "사용: ccdiff [--list | --verify] [세션ID | 세션.jsonl]\n인자가 없으면 현재 디렉터리 프로젝트의 가장 최근 세션을 연다.\n--list 는 화면 없이 수정 목록만 출력한다.\n--verify 는 되살린 파일이 다음 수정의 원본과 같은지 검사한다."
        );
        return Ok(());
    }
    let list_only = args.iter().any(|a| a == "--list");
    args.retain(|a| a != "--list");
    let path = resolve_session(args.into_iter().next())?;
    let session = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();

    let mut app = App {
        session,
        tail: Tail::new(path),
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
        rows: HashMap::new(),
        file: None,
        git: GitIndex::default(),
        commits: HashMap::new(),
        commit_filter: 0,
        wt_roots: Vec::new(),
        wt: Vec::new(),
        wt_at: None,
        session_start: 0,
        show_old_wt: false,
    };
    app.ingest()?;
    app.refresh_commits();
    app.refresh_worktrees();
    app.select_last();

    // 되살린 "수정 후 파일" 이 같은 파일의 다음 수정의 "수정 전 파일" 과 같은지 본다.
    if args_verify() {
        let (mut ok, mut bad) = (0, 0);
        for i in 0..app.entries.len() {
            let Some(j) = (i + 1..app.entries.len()).find(|&j| app.entries[j].path == app.entries[i].path) else { continue };
            let Some(next_before) = app.entries[j].before.clone() else { continue };
            app.ensure_rows(i);
            let after: Vec<String> = app.rows[&i]
                .iter()
                .filter(|r| r.kind != RowKind::Del)
                .map(|r| r.spans.iter().map(|(_, t)| t.as_str()).collect())
                .collect();
            let expect: Vec<&str> = next_before.lines().collect();
            if after == expect {
                ok += 1;
            } else {
                bad += 1;
                let at = after.iter().zip(&expect).position(|(a, b)| a != b);
                println!("불일치: {} {} (줄 {:?}, {}줄 vs {}줄)", app.entries[i].time, app.entries[i].short_path(), at.map(|x| x + 1), after.len(), expect.len());
            }
        }
        // 작업 트리 항목: HEAD 에 git diff 를 적용한 결과가 디스크의 지금 파일과 같아야 한다.
        app.refresh_worktrees();
        for j in 0..app.wt.len() {
            let key = WT + j;
            let Ok(disk) = fs::read_to_string(&app.wt[j].path) else { continue };
            app.ensure_rows(key);
            let after: Vec<String> = app.rows[&key]
                .iter()
                .filter(|r| r.kind != RowKind::Del)
                .map(|r| r.spans.iter().map(|(_, t)| t.as_str()).collect())
                .collect();
            if after == disk.lines().collect::<Vec<_>>() {
                ok += 1;
            } else {
                bad += 1;
                println!("불일치(작업 트리): {}", app.wt[j].short_path());
            }
        }
        println!("검증: 일치 {ok} · 불일치 {bad}");
        return Ok(());
    }

    if list_only {
        for (i, e) in app.visible().into_iter().map(|i| (i, app.entry(i))) {
            let c = match app.commits.get(&i) {
                _ if i >= WT => "◆ 작업트리".into(),
                Some(CommitState::Yes { hash, .. }) => format!("✓ {hash}"),
                Some(CommitState::No) => "● 미커밋".into(),
                Some(CommitState::Changed { hash, .. }) => format!("◐ {hash}"),
                Some(CommitState::Gone) => "✗ 없음".into(),
                _ => "?".into(),
            };
            println!("{} {:<9} +{:<4} -{:<4} {:<10} {}", e.time, e.kind, e.added, e.removed, c, e.short_path());
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
