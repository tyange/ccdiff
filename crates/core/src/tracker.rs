//! 세션 하나의 기록을 따라 읽는다: 처음 연 세션, compact 로 이어진 세션, 그 서브에이전트들.
//! 새로 읽은 수정·워크트리·cwd 는 `Changes` 로 돌려준다 — IDE 폴더 추가나 화면 갱신은 쓰는 쪽이 한다.

use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::session::{self, bash_roots, parse_entry, EditEntry, Tail};

/// 마지막으로 거둔 뒤 새로 생긴 것.
#[derive(Default)]
pub struct Changes {
    /// 새 수정의 `entries` 인덱스.
    pub entries: Range<usize>,
    /// 새로 다루기 시작한 워크트리(수정했거나 Bash 명령에 경로가 나온 git 루트).
    pub roots: Vec<PathBuf>,
    /// 기록 줄에 나온 cwd(처음 본 것만).
    pub cwds: Vec<PathBuf>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.roots.is_empty() && self.cwds.is_empty()
    }
}

pub struct SessionTracker {
    tails: Vec<Tail>,
    /// 세션 본 기록 사슬(처음 연 것부터). 서브에이전트는 빠진다.
    chain: Vec<PathBuf>,
    /// 이미 읽거나 이어짐이 아니라고 본 기록 파일.
    known: HashSet<PathBuf>,
    /// 읽은 줄의 uuid — 이어진 세션은 앞 세션 줄을 옮겨 담아 시작하므로 겹치는 줄을 거른다.
    seen: HashSet<String>,
    found_at: Option<Instant>,
    roots: HashMap<PathBuf, Option<PathBuf>>,
    cwds: HashSet<PathBuf>,
    pub entries: Vec<EditEntry>,
    /// git diff 를 볼 워크트리(세션이 고쳤거나 Bash 로 다룬 곳).
    pub wt_roots: Vec<PathBuf>,
    /// 세션 첫 기록 시각.
    pub session_start: i64,
    /// 마지막 기록 시각.
    pub last_epoch: i64,
    /// 세션이 처음 돈 디렉터리 — Claude 가 IDE 후보를 고를 때 이것을 품는 workspaceFolders 만 보여 준다.
    pub cwd: Option<PathBuf>,
    /// 아직 거두지 않은 변화.
    pending: Changes,
}

impl SessionTracker {
    pub fn new(path: PathBuf) -> Self {
        Self {
            tails: vec![Tail::new(path.clone())],
            chain: vec![path.clone()],
            known: HashSet::from([path]),
            seen: HashSet::new(),
            found_at: None,
            roots: HashMap::new(),
            cwds: HashSet::new(),
            entries: Vec::new(),
            wt_roots: Vec::new(),
            session_start: 0,
            last_epoch: 0,
            cwd: None,
            pending: Changes::default(),
        }
    }

    /// 처음 연 기록 파일.
    pub fn path(&self) -> &Path {
        &self.chain[0]
    }

    /// 화면 제목: 마지막 기록의 세션 ID. 이어졌으면 몇 번 이어졌는지 붙인다.
    pub fn label(&self) -> String {
        let stem = |p: &PathBuf| p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let last = stem(self.chain.last().expect("chain"));
        match self.chain.len() {
            1 => last,
            n => format!("{last} (compact 로 {}번 이어짐)", n - 1),
        }
    }

    /// 이어진 기록까지 포함한 본 기록 사슬.
    pub fn chain(&self) -> &[PathBuf] {
        &self.chain
    }

    /// 새 줄을 읽는다. 처음 연 기록을 못 읽으면 오류.
    pub fn ingest(&mut self) -> Result<()> {
        let mut lines = Vec::new();
        for (i, t) in self.tails.iter_mut().enumerate() {
            match t.read_lines() {
                Ok(l) => lines.extend(l),
                Err(e) if i == 0 => return Err(e),
                Err(_) => {}
            }
        }
        for line in lines {
            if let Some(u) = session::line_uuid(&line) {
                if !self.seen.insert(u) {
                    continue;
                }
            }
            let at = session::line_epoch(&line);
            if self.session_start == 0 {
                self.session_start = at;
            }
            self.last_epoch = self.last_epoch.max(at);
            if let Some(cwd) = session::line_cwd(&line) {
                if self.cwds.insert(cwd.clone()) {
                    self.pending.cwds.push(cwd.clone());
                }
                self.cwd.get_or_insert(cwd);
            }
            for r in bash_roots(&line, &mut self.roots) {
                self.add_root(r);
            }
            if let Some(e) = parse_entry(&line, &mut self.roots) {
                if let Some(r) = e.root.clone() {
                    self.add_root(r);
                }
                if self.pending.entries.is_empty() {
                    self.pending.entries = self.entries.len()..self.entries.len();
                }
                self.entries.push(e);
                self.pending.entries.end = self.entries.len();
            }
        }
        Ok(())
    }

    fn add_root(&mut self, r: PathBuf) {
        if !self.wt_roots.contains(&r) {
            self.wt_roots.push(r.clone());
            self.pending.roots.push(r);
        }
    }

    /// 마지막으로 거둔 뒤 새로 생긴 것을 넘겨받는다.
    pub fn take_changes(&mut self) -> Changes {
        std::mem::take(&mut self.pending)
    }

    fn add_tail(&mut self, p: PathBuf) {
        self.known.insert(p.clone());
        self.tails.push(Tail::new(p));
    }

    /// 새로 생긴 서브에이전트 기록과 compact 로 이어진 세션 기록을 찾아 읽기 목록에 넣는다.
    pub fn discover(&mut self) -> bool {
        let before = self.tails.len();
        let dir = self.chain[0].parent().map(PathBuf::from).unwrap_or_default();
        // 사슬이 길어지면 새 꼬리가 또 이어질 수 있어 더 찾을 게 없을 때까지 돈다.
        loop {
            let mut next = None;
            for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "jsonl") || self.known.contains(&p) {
                    continue;
                }
                match session::compact_parent(&p) {
                    Some(u) if self.seen.contains(&u) => {
                        next = Some(p);
                        break;
                    }
                    Some(_) => {
                        self.known.insert(p);
                    }
                    // 막 생긴 파일은 경계 줄이 아직 안 써졌을 수 있다 — 조금 지난 것만 제외한다.
                    None => {
                        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
                        if old.is_some_and(|d| d > Duration::from_secs(30)) {
                            self.known.insert(p);
                        }
                    }
                }
            }
            let Some(p) = next else { break };
            self.chain.push(p.clone());
            self.add_tail(p);
            // 이어진 기록의 줄을 읽어야 그 다음 이어짐의 부모 uuid 를 알 수 있다.
            let _ = self.ingest();
        }
        for s in self.chain.clone() {
            let dir = s.with_extension("").join("subagents");
            for e in fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "jsonl") && !self.known.contains(&p) {
                    self.add_tail(p);
                }
            }
        }
        self.tails.len() > before
    }

    /// 5초에 한 번만 찾는다 — 프로젝트 폴더 전체를 훑기 때문이다.
    pub fn maybe_discover(&mut self) {
        if self.found_at.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.found_at = Some(Instant::now());
        self.discover();
    }

    /// 처음 열 때: 읽고, 이어진 기록을 찾고, 다시 읽는다.
    pub fn load(&mut self) -> Result<()> {
        self.ingest()?;
        self.discover();
        self.ingest()
    }
}
