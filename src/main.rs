mod bg;
mod git;
mod highlight;
mod ide;
mod replay;
mod rows;
mod session;
mod ui;
mod worktree;

use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use ratatui::{
    crossterm::{
        event::{
            self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
            MouseButton, MouseEvent, MouseEventKind,
        },
        execute,
    },
    layout::{Position, Rect},
    widgets::ListState,
    DefaultTerminal,
};

use bg::{run_git, GitDone, GitReq, GitWorker, HlJob, HlWorker};
use git::{query, CommitState, GitIndex};
use highlight::Highlighter;
use ide::Ide;
use replay::{Replay, Step};
use rows::{apply_highlight, build_rows, Row, RowKind};
use session::{bash_roots, parse_entry, EditEntry, Tail};

/// 작업 트리 항목의 키 시작값. 세션 수정은 0부터, 작업 트리 항목은 WT 부터 센다.
pub const WT: usize = 1 << 40;
const WT_REFRESH: Duration = Duration::from_secs(10);

/// 목록의 한 줄: 워크트리 머리줄(선택 안 됨) 또는 항목.
pub enum Disp {
    Header { key: usize, count: usize, collapsed: bool },
    Item(usize),
}

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

/// diff 에서 고른 줄 범위(줄 목록 인덱스). anchor 는 고르기 시작한 줄.
pub struct Sel {
    pub key: usize,
    pub anchor: usize,
    pub head: usize,
}

impl Sel {
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
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
    hlw: HlWorker,
    gitw: GitWorker,
    git_busy: bool,
    /// 하이라이트 세대. 테마가 바뀌면 올라가고, 옛 결과는 버린다.
    gen: u64,
    /// 작업 트리 세대. 다시 읽을 때마다 올라간다 — 같은 키가 다른 파일을 가리킬 수 있어서.
    wt_gen: u64,
    /// 색을 기다리는 항목.
    pub hl_wait: HashSet<usize>,
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
    /// 펼친 워크트리 그룹. 기본은 접힘이고, 펼친 것만 실행 사이에 기억한다.
    expanded: HashSet<Option<PathBuf>>,
    pub sel: Option<Sel>,
    dragging: bool,
    /// 그리기 때 채운다: diff 칸 안쪽 화면 줄마다 (항목 키, 줄 인덱스).
    pub diff_rows: Vec<Option<(usize, usize)>>,
    pub ide: Option<Ide>,
    /// Claude 에 마지막으로 보낸 선택 줄 수(상태줄 표시용).
    pub sent: Option<usize>,
    /// 세션이 돈 디렉터리 — Claude 가 IDE 후보를 고를 때 이것을 품는 workspaceFolders 만 보여 준다.
    cwd: Option<PathBuf>,
}

fn same_change(a: &EditEntry, b: &EditEntry) -> bool {
    a.path == b.path
        && a.kind == b.kind
        && a.before == b.before
        && a.hunks.len() == b.hunks.len()
        && a.hunks.iter().zip(&b.hunks).all(|(x, y)| x.old_start == y.old_start && x.lines == y.lines)
}

/// 펼친 그룹 목록 파일. 한 줄에 워크트리 경로 하나, git 밖은 `-`.
fn state_file() -> Option<PathBuf> {
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("ccdiff/expanded"))
}

fn load_expanded() -> HashSet<Option<PathBuf>> {
    let Some(f) = state_file() else { return HashSet::new() };
    fs::read_to_string(f)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| if l == "-" { None } else { Some(PathBuf::from(l)) })
        .collect()
}

impl App {
    fn ingest(&mut self) -> Result<bool> {
        let mut changed = false;
        for line in self.tail.read_lines()? {
            if self.session_start == 0 {
                self.session_start = session::line_epoch(&line);
            }
            if let Some(cwd) = session::line_cwd(&line) {
                if let Some(ide) = self.ide.as_mut() {
                    ide.add_folder(&cwd);
                }
                self.cwd.get_or_insert(cwd);
            }
            for r in bash_roots(&line, &mut self.roots) {
                if !self.wt_roots.contains(&r) {
                    if let Some(ide) = self.ide.as_mut() {
                        ide.add_folder(&r);
                    }
                    self.wt_roots.push(r);
                    self.wt_at = None;
                }
            }
            if let Some(e) = parse_entry(&line, &mut self.roots) {
                if let Some(r) = &e.root {
                    if !self.wt_roots.contains(r) {
                        if let Some(ide) = self.ide.as_mut() {
                            ide.add_folder(r);
                        }
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

    /// 줄 목록은 바로 만들고 색은 백그라운드에 맡긴다 — 큰 파일에서 방향키가 끊기지 않게.
    pub fn ensure_rows(&mut self, key: usize) {
        if self.rows.contains_key(&key) {
            return;
        }
        let (rows, src) = build_rows(self.entry(key), &self.hl);
        self.rows.insert(key, rows);
        if let Some((before, after)) = src {
            let job = HlJob { key, gen: self.tag(key), path: self.entry(key).path.clone(), before, after, sel: self.hl.selection() };
            self.hlw.push(job);
            self.hl_wait.insert(key);
        }
    }

    fn tag(&self, key: usize) -> u64 {
        (self.gen << 32) | if key >= WT { self.wt_gen } else { 0 }
    }

    /// 테마가 바뀌었을 때: 만든 줄을 버리고 다시 칠하게 한다.
    fn reset_highlight(&mut self) {
        self.gen += 1;
        self.rows.clear();
        self.hl_wait.clear();
        self.hlw.drop_older(self.gen << 32);
    }

    /// 백그라운드 결과를 받아 반영한다.
    fn poll_bg(&mut self) {
        while let Ok(d) = self.hlw.rx.try_recv() {
            if d.gen != self.tag(d.key) {
                continue;
            }
            if let Some(rows) = self.rows.get_mut(&d.key) {
                apply_highlight(rows, &d.before, &d.after);
            }
            self.hl_wait.remove(&d.key);
        }
        while let Ok(done) = self.gitw.rx.try_recv() {
            self.git_busy = false;
            self.apply_git(done);
        }
    }

    fn git_req(&self) -> GitReq {
        let since = self.entries.iter().map(|e| e.epoch).filter(|&t| t > 0).min().unwrap_or(0) - 60;
        // 커밋됨으로 판정된 것은 바뀌지 않으므로 다시 묻지 않는다.
        let queries = (0..self.entries.len())
            .filter(|i| !matches!(self.commits.get(i), Some(CommitState::Yes { .. } | CommitState::Changed { .. })))
            .map(|i| query(i, &self.entries[i]))
            .collect();
        GitReq { roots: self.wt_roots.clone(), queries, since }
    }

    fn request_git(&mut self) {
        if self.git_busy || self.wt_at.is_some_and(|t| t.elapsed() < WT_REFRESH) {
            return;
        }
        self.wt_at = Some(Instant::now());
        self.gitw.send(self.git_req());
        self.git_busy = true;
    }

    /// --list·--verify 용: 기다렸다가 바로 반영한다.
    fn sync_git(&mut self) {
        let done = run_git(&mut GitIndex::default(), &self.git_req());
        self.apply_git(done);
    }

    /// 새로 읽은 작업 트리·커밋 판정을 반영한다. 열려 있는 파일 보기의 작업 트리 키도 새 번호로 맞춘다.
    fn apply_git(&mut self, done: GitDone) {
        for (k, st) in done.commits {
            self.commits.insert(k, st);
        }
        // 내용이 그대로인 항목은 이미 만든 줄과 색을 새 키로 옮긴다 — 10초마다 색이 깜빡이지 않게.
        let old = std::mem::take(&mut self.wt);
        let mut keep: HashMap<usize, Vec<Row>> = HashMap::new();
        for (j, e) in done.wt.iter().enumerate() {
            let Some(oj) = old.iter().position(|o| same_change(o, e)) else { continue };
            if self.hl_wait.contains(&(WT + oj)) {
                continue;
            }
            if let Some(r) = self.rows.remove(&(WT + oj)) {
                keep.insert(WT + j, r);
            }
        }
        self.rows.retain(|&k, _| k < WT);
        self.hl_wait.retain(|&k| k < WT);
        self.rows.extend(keep);
        self.wt_gen += 1;
        self.wt = done.wt;
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

    /// 워크트리별로 묶은 목록. 최근에 손댄 워크트리가 아래로 간다(따라가기가 맨 아래를 고른다).
    pub fn display(&self) -> Vec<Disp> {
        let mut groups: Vec<(Option<PathBuf>, Vec<usize>)> = Vec::new();
        for k in self.visible() {
            let root = self.entry(k).root.clone();
            match groups.iter_mut().find(|g| g.0 == root) {
                Some(g) => g.1.push(k),
                None => groups.push((root, vec![k])),
            }
        }
        groups.sort_by_key(|g| g.1.iter().map(|&k| self.entry(k).epoch).max());
        let mut out = Vec::new();
        for (root, keys) in groups {
            let collapsed = !self.expanded.contains(&root);
            out.push(Disp::Header { key: keys[0], count: keys.len(), collapsed });
            if !collapsed {
                out.extend(keys.into_iter().map(Disp::Item));
            }
        }
        out
    }

    /// 선택한 줄이 속한 그룹을 접거나 펴고, 그 그룹 머리줄을 선택한다.
    fn toggle_group(&mut self) {
        let d = self.display();
        let Some(key) = self.list.selected().and_then(|s| d.get(s)).map(|x| match x {
            Disp::Header { key, .. } | Disp::Item(key) => *key,
        }) else {
            return;
        };
        let root = self.entry(key).root.clone();
        if !self.expanded.remove(&root) {
            self.expanded.insert(root.clone());
        }
        self.save_expanded();
        let d = self.display();
        let at = d.iter().position(|x| matches!(x, Disp::Header { key, .. } if self.entry(*key).root == root));
        // 목록 길이가 바뀌므로 스크롤 위치를 처음부터 다시 잡게 한다(선택 줄은 그리기 때 보이게 맞춰진다).
        *self.list.offset_mut() = 0;
        self.list.select(at);
        self.follow = false;
        self.scroll = 0;
    }

    fn toggle_all_groups(&mut self) {
        let roots: HashSet<Option<PathBuf>> = self.visible().into_iter().map(|k| self.entry(k).root.clone()).collect();
        if roots.iter().any(|r| self.expanded.contains(r)) {
            self.expanded.retain(|r| !roots.contains(r));
        } else {
            self.expanded.extend(roots);
        }
        self.save_expanded();
        *self.list.offset_mut() = 0;
        self.follow = true;
        self.select_last();
    }

    /// 저장 실패는 무시한다 — 기억 못 하는 것뿐 화면 동작에는 지장이 없다.
    fn save_expanded(&self) {
        let Some(f) = state_file() else { return };
        let mut lines: Vec<String> = self
            .expanded
            .iter()
            .map(|r| r.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "-".into()))
            .collect();
        lines.sort();
        if let Some(dir) = f.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(f, lines.join("\n") + "\n");
    }

    pub fn on_header(&self) -> bool {
        let d = self.display();
        matches!(self.list.selected().and_then(|s| d.get(s)), Some(Disp::Header { .. }))
    }

    pub fn selected_entry(&self) -> Option<usize> {
        match self.list.selected().and_then(|s| self.display().into_iter().nth(s)) {
            Some(Disp::Item(k)) => Some(k),
            _ => None,
        }
    }

    /// 선택 줄이 가리키는 것 — 키는 갱신 때 바뀔 수 있어 내용으로 비교한다.
    fn selected_id(&self) -> Option<(bool, PathBuf, i64, &'static str)> {
        let d = self.display();
        let (header, key) = match self.list.selected().and_then(|s| d.get(s))? {
            Disp::Header { key, .. } => (true, *key),
            Disp::Item(key) => (false, *key),
        };
        let e = self.entry(key);
        Some((header, e.path.clone(), e.epoch, e.kind))
    }

    /// 따라가기는 주기적으로 불린다 — 선택이 그대로면 읽던 스크롤을 두고, 바뀔 때만 맨 위로 보낸다.
    fn select_last(&mut self) {
        let before = self.selected_id();
        let n = self.display().len();
        self.list.select(n.checked_sub(1));
        if self.selected_id() != before {
            self.scroll = 0;
        }
    }

    fn move_by(&mut self, delta: isize) {
        let n = self.display().len();
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

    fn row_at(&self, y: u16) -> Option<(usize, usize)> {
        let i = y.checked_sub(self.diff_area.y + 1)? as usize;
        self.diff_rows.get(i).copied().flatten()
    }

    /// 드래그로 줄을 고른다. 손을 떼면 Claude 에 보낸다.
    /// 화면 맨 위 줄부터 고른다(첫 변경 줄이 보이면 그 줄).
    fn start_key_selection(&mut self) {
        let shown: Vec<(usize, usize)> = self.diff_rows.iter().flatten().copied().collect();
        let Some(&(key, top)) = shown.first() else { return };
        let rows = &self.rows[&key];
        let j = shown.iter().map(|&(_, j)| j).find(|&j| rows[j].is_change()).unwrap_or(top);
        self.sel = Some(Sel { key, anchor: j, head: j });
        self.send_selection();
    }

    fn on_select_mouse(&mut self, m: &MouseEvent, pos: Position) -> bool {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) if self.diff_area.contains(pos) => {
                if let Some((key, j)) = self.row_at(m.row) {
                    self.sel = Some(Sel { key, anchor: j, head: j });
                    self.dragging = true;
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                let y = m.row.clamp(self.diff_area.y + 1, (self.diff_area.y + self.diff_area.height).saturating_sub(2));
                if let (Some((key, j)), Some(s)) = (self.row_at(y), self.sel.as_mut()) {
                    if s.key == key {
                        s.head = j;
                    }
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.dragging => {
                self.dragging = false;
                self.send_selection();
                true
            }
            _ => false,
        }
    }

    /// 고른 줄을 Claude Code 에 알린다. 줄 번호는 그 수정 직후 파일 기준이다.
    fn send_selection(&mut self) {
        let Some(s) = &self.sel else { return };
        let Some(rows) = self.rows.get(&s.key) else { return };
        let (a, b) = s.range();
        let picked: Vec<&Row> = rows[a..=b.min(rows.len() - 1)].iter().filter(|r| r.kind != RowKind::Sep).collect();
        if picked.is_empty() {
            return;
        }
        let mixed = picked.iter().any(|r| r.kind == RowKind::Del) && picked.iter().any(|r| r.kind != RowKind::Del);
        let text: Vec<String> = picked
            .iter()
            .map(|r| {
                let t: String = r.spans.iter().map(|(_, t)| t.as_str()).collect();
                if !mixed {
                    return t;
                }
                let mark = match r.kind {
                    RowKind::Add => '+',
                    RowKind::Del => '-',
                    _ => ' ',
                };
                format!("{mark}{t}")
            })
            .collect();
        let nos: Vec<usize> = picked.iter().filter_map(|r| r.new_no).collect();
        let nos = if nos.is_empty() { picked.iter().filter_map(|r| r.old_no).collect() } else { nos };
        let (Some(&lo), Some(&hi)) = (nos.iter().min(), nos.iter().max()) else { return };
        let path = self.entry(s.key).path.clone();
        if let Some(ide) = &self.ide {
            ide.select(&path, Some((lo - 1, hi - 1)), &text.join("\n"));
            self.sent = Some(text.len());
        }
    }

    fn clear_selection(&mut self) -> bool {
        let Some(s) = self.sel.take() else { return false };
        if let Some(ide) = &self.ide {
            ide.select(&self.entry(s.key).path, None, "");
        }
        self.sent = None;
        true
    }

    /// 파일 보기에서 키보드로 고르기: v 로 시작하면 j/k 가 선택 끝을 움직인다.
    fn move_head(&mut self, delta: isize) {
        let Some(s) = self.sel.as_mut() else { return };
        let n = self.rows.get(&s.key).map(|r| r.len()).unwrap_or(0);
        s.head = (s.head as isize + delta).clamp(0, n as isize - 1) as usize;
        let head = s.head;
        if let Some(fv) = self.file.as_mut() {
            let h = self.diff_area.height.saturating_sub(2) as usize;
            if head < fv.scroll as usize {
                fv.scroll = head as u16;
            } else if head >= fv.scroll as usize + h {
                fv.scroll = (head + 1 - h) as u16;
            }
        }
        self.send_selection();
    }

    fn on_mouse(&mut self, m: &MouseEvent) {
        let pos = Position::new(m.column, m.row);
        if self.on_select_mouse(m, pos) {
            return;
        }
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
                let d = self.display();
                if idx < d.len() {
                    self.list.select(Some(idx));
                    self.follow = idx == d.len() - 1;
                    self.scroll = 0;
                    if matches!(d[idx], Disp::Header { .. }) {
                        self.toggle_group();
                    }
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
                self.reset_highlight();
                return false;
            }
            KeyCode::Char('b') => {
                self.hl.light = !self.hl.light;
                self.reset_highlight();
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
            KeyCode::Esc if self.clear_selection() => {}
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Enter | KeyCode::Char('o') if self.on_header() => self.toggle_group(),
            KeyCode::Enter | KeyCode::Char('o') => self.open_file(),
            KeyCode::Char(' ') | KeyCode::Char('z') => self.toggle_group(),
            KeyCode::Char('Z') => self.toggle_all_groups(),
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
        let selecting = self.sel.is_some() && self.file.as_ref().is_some_and(|v| v.replay.is_none());
        match k.code {
            KeyCode::Esc if self.clear_selection() => return,
            KeyCode::Char('v') => {
                if !self.clear_selection() {
                    self.start_key_selection();
                }
                return;
            }
            KeyCode::Char('j') | KeyCode::Down if selecting => return self.move_head(1),
            KeyCode::Char('k') | KeyCode::Up if selecting => return self.move_head(-1),
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
                app.reset_highlight();
            }
        }
        app.tick_replay();
        app.poll_bg();
        if !app.replaying() {
            app.request_git();
        }
        terminal.draw(|f| ui::draw(f, app))?;
        let busy = app.replaying() || !app.hl_wait.is_empty() || app.git_busy;
        let wait = if busy { 30 } else { 300 };
        if event::poll(Duration::from_millis(wait))? {
            // 키를 누르고 있으면 입력이 쌓인다 — 쌓인 만큼 처리한 뒤 한 번만 그린다.
            for _ in 0..64 {
                match event::read()? {
                    Event::Mouse(m) => app.on_mouse(&m),
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        if app.on_key(k) {
                            return Ok(());
                        }
                    }
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
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
        hlw: HlWorker::spawn(),
        gitw: GitWorker::spawn(),
        git_busy: false,
        gen: 0,
        wt_gen: 0,
        hl_wait: HashSet::new(),
        commits: HashMap::new(),
        commit_filter: 0,
        wt_roots: Vec::new(),
        wt: Vec::new(),
        wt_at: None,
        session_start: 0,
        show_old_wt: false,
        expanded: load_expanded(),
        sel: None,
        dragging: false,
        diff_rows: Vec::new(),
        ide: None,
        sent: None,
        cwd: None,
    };
    app.ingest()?;
    if list_only || args_verify() {
        app.sync_git();
    }
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

    // 화면을 띄울 때만 연다 — --list·--verify 가 잠금 파일을 남기지 않게.
    if env::var_os("CCDIFF_NO_IDE").is_none() {
        app.ide = Ide::start(app.cwd.iter().chain(&app.wt_roots).cloned().collect());
    }
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
