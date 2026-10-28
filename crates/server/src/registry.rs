//! 세션 목록 상태. 갱신 스레드가 주기적으로 고치고, API 는 잠깐 잠가 읽는다.
//! 오래 걸리는 일(큰 기록 처음 읽기, git)은 잠금 밖에서 하고 결과만 잠가 넣는다 — API 가 막히지 않게.

use std::{
    collections::{hash_map::Entry as MapEntry, HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{json, Value};

use ccdiff_core::{
    git::{query, CommitQuery, CommitState, GitIndex},
    ide::Ide,
    session::{git_root, EditEntry},
    tracker::SessionTracker,
    worktree,
};

use crate::{
    discover::{self, Found},
    procs::Procs,
    tmux::Tmux,
};

const GIT_REFRESH: Duration = Duration::from_secs(10);

pub struct Entry {
    pub found: Found,
    pub tracker: Option<SessionTracker>,
    pub commits: HashMap<usize, CommitState>,
}

impl Entry {
    /// 세션이 다룬 워크트리들. 기록이 없으면 cwd 의 git 루트.
    pub fn roots(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self.tracker.as_ref().map(|t| t.wt_roots.clone()).unwrap_or_default();
        if out.is_empty() {
            out.extend(git_root(&self.cwd()));
        }
        out
    }

    pub fn cwd(&self) -> PathBuf {
        self.found.file.as_ref().map(|s| s.cwd.clone()).unwrap_or_else(|| self.found.pane.path.clone())
    }

    /// 대표 워크트리: 마지막으로 고친 파일의 워크트리, 없으면 마지막으로 다룬 워크트리, 없으면 cwd 의 git 루트.
    fn primary(&self) -> Option<PathBuf> {
        let t = self.tracker.as_ref();
        t.and_then(|t| t.entries.iter().rev().find_map(|x| x.root.clone()).or_else(|| t.wt_roots.last().cloned()))
            .or_else(|| git_root(&self.cwd()))
    }
}

/// 워크트리 하나의 git 정보. 갱신 스레드가 10초마다 다시 읽는다.
#[derive(Default)]
pub struct WtInfo {
    pub branch: String,
    /// `git status --porcelain` 줄 수(untracked 폴더는 한 줄).
    pub dirty: usize,
    /// `git diff HEAD` 와 untracked 파일(ccdiff 의 ◆ 항목).
    pub items: Vec<EditEntry>,
}

/// 세션에 보낸 마지막 선택 — 나중에 붙는 Claude 에 다시 보낸다.
#[derive(Clone)]
pub struct Selection {
    pub path: PathBuf,
    pub lines: (usize, usize),
    pub text: String,
}

#[derive(Default)]
pub struct Registry {
    pub entries: HashMap<String, Entry>,
    pub wt: HashMap<PathBuf, WtInfo>,
    /// IDE 연결 번호 → 상대 프로세스 pid.
    ide_pids: HashMap<u64, u32>,
    /// 마지막 선택을 이미 다시 보낸 연결.
    greeted: HashSet<u64>,
    pub last_sel: HashMap<String, Selection>,
}

pub type Shared = Arc<Mutex<Registry>>;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `repo-gh123` → 123. 폴더 이름에 없으면 브랜치 이름의 `gh123`·`GH-123` 에서 찾는다.
pub fn issue_number(root: &Path, branch: &str) -> Option<u32> {
    let digits_after = |s: &str, pat: &str| -> Option<u32> {
        let lower = s.to_ascii_lowercase();
        let i = lower.rfind(pat)? + pat.len();
        let d: String = lower[i..].trim_start_matches('-').chars().take_while(char::is_ascii_digit).collect();
        d.parse().ok()
    };
    let name = root.file_name()?.to_string_lossy();
    digits_after(&name, "-gh").or_else(|| digits_after(branch, "gh"))
}

/// IDE 연결의 상대 프로세스. 그 포트를 쓰는 연결 중 이 서버가 아닌 쪽.
fn peer_pid(port: u16) -> Option<u32> {
    let out = Command::new("lsof").args(["-nP", "-a", &format!("-iTCP:{port}"), "-sTCP:ESTABLISHED", "-Fp"]).output().ok()?;
    let me = std::process::id();
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.strip_prefix('p')?.parse().ok()).find(|&p| p != me)
}

/// 갱신 스레드가 들고 있는 것.
pub struct Refresher {
    pub tmux: Tmux,
    pub ide: Option<Arc<Mutex<Ide>>>,
    git: GitIndex,
    git_at: Option<Instant>,
}

impl Refresher {
    pub fn new(tmux: Tmux, ide: Option<Arc<Mutex<Ide>>>) -> Self {
        Self { tmux, ide, git: GitIndex::default(), git_at: None }
    }

    pub fn refresh(&mut self, shared: &Shared) {
        let panes = self.tmux.panes();
        let procs = Procs::snapshot();
        let found = discover::find(&panes, &procs);

        // 이미 있는 세션은 이어 읽고(빠르다), 새 세션·기록이 바뀐 세션은 잠금 밖에서 처음부터 읽는다.
        let to_load: Vec<Found> = {
            let Ok(mut reg) = shared.lock() else { return };
            let keys: HashSet<String> = found.iter().map(Found::key).collect();
            reg.entries.retain(|k, _| keys.contains(k));
            let mut fresh = Vec::new();
            for f in found {
                let key = f.key();
                match reg.entries.get_mut(&key) {
                    Some(e) if e.found.jsonl == f.jsonl => {
                        e.found = f;
                        if let Some(t) = e.tracker.as_mut() {
                            t.maybe_discover();
                            let _ = t.ingest();
                            t.take_changes();
                        }
                    }
                    _ => fresh.push(f),
                }
            }
            fresh
        };
        let loaded: Vec<(String, Entry)> = to_load
            .into_iter()
            .map(|f| {
                let tracker = f.jsonl.clone().and_then(|p| {
                    let mut t = SessionTracker::new(p);
                    t.load().ok().map(|_| t)
                });
                (f.key(), Entry { found: f, tracker, commits: HashMap::new() })
            })
            .collect();
        let new_sessions = !loaded.is_empty();
        if let Ok(mut reg) = shared.lock() {
            reg.entries.extend(loaded);
        }
        if new_sessions || self.git_at.is_none_or(|t| t.elapsed() >= GIT_REFRESH) {
            self.git_at = Some(Instant::now());
            self.refresh_git(shared);
        }
        self.refresh_ide(shared);
    }

    /// 커밋 판정·작업 트리·브랜치를 다시 읽는다. 물을 것만 잠가 뽑고, git 은 잠금 밖에서 부른다.
    fn refresh_git(&mut self, shared: &Shared) {
        let (roots, queries): (HashSet<PathBuf>, Vec<(String, i64, CommitQuery)>) = {
            let Ok(reg) = shared.lock() else { return };
            let mut roots = HashSet::new();
            let mut queries = Vec::new();
            for (key, e) in &reg.entries {
                roots.extend(e.roots());
                roots.extend(e.primary());
                let Some(t) = &e.tracker else { continue };
                let since = t.entries.iter().map(|x| x.epoch).filter(|&x| x > 0).min().unwrap_or(0) - 60;
                for (i, x) in t.entries.iter().enumerate() {
                    // 커밋됨으로 판정된 것은 바뀌지 않으므로 다시 묻지 않는다.
                    if !matches!(e.commits.get(&i), Some(CommitState::Yes { .. } | CommitState::Changed { .. })) {
                        queries.push((key.clone(), since, query(i, x)));
                    }
                }
            }
            (roots, queries)
        };
        self.git.expire();
        let states: Vec<(String, usize, CommitState)> =
            queries.iter().map(|(k, since, q)| (k.clone(), q.key, self.git.state(q, *since))).collect();
        let infos: Vec<(PathBuf, WtInfo)> = roots
            .into_iter()
            .map(|r| {
                let branch = git(&r, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_default();
                let dirty = git(&r, &["status", "--porcelain"]).map(|s| s.lines().count()).unwrap_or(0);
                let items = worktree::entries(&r);
                (r, WtInfo { branch, dirty, items })
            })
            .collect();
        let Ok(mut reg) = shared.lock() else { return };
        for (k, i, st) in states {
            if let Some(e) = reg.entries.get_mut(&k) {
                e.commits.insert(i, st);
            }
        }
        reg.wt = infos.into_iter().collect();
    }

    /// IDE 연결을 세션과 짝짓고, 새로 준비된 연결에 그 세션의 마지막 선택을 다시 보낸다.
    /// 워크스페이스 폴더에는 세션들의 cwd·워크트리를 넣는다 — Claude 는 자기 cwd 를 품는 IDE 만 `/ide` 에 보인다.
    fn refresh_ide(&mut self, shared: &Shared) {
        let Some(ide) = &self.ide else { return };
        let Ok(mut ide) = ide.lock() else { return };
        let clients = ide.clients();
        let Ok(mut reg) = shared.lock() else { return };
        let live: HashSet<u64> = clients.iter().map(|c| c.id).collect();
        reg.ide_pids.retain(|id, _| live.contains(id));
        reg.greeted.retain(|id| live.contains(id));
        let folders: Vec<PathBuf> = reg.entries.values().flat_map(|e| std::iter::once(e.cwd()).chain(e.roots())).collect();
        for f in folders {
            ide.add_folder(&f);
        }
        for c in &clients {
            if let MapEntry::Vacant(v) = reg.ide_pids.entry(c.id) {
                if let Some(pid) = peer_pid(c.peer.port()) {
                    v.insert(pid);
                }
            }
            if !c.ready || reg.greeted.contains(&c.id) {
                continue;
            }
            let Some(&pid) = reg.ide_pids.get(&c.id) else { continue };
            reg.greeted.insert(c.id);
            let key = reg.entries.iter().find(|(_, e)| e.found.pid == pid).map(|(k, _)| k.clone());
            if let Some(sel) = key.and_then(|k| reg.last_sel.get(&k)) {
                ide.select_to(&[c.id], &sel.path, Some(sel.lines), &sel.text);
            }
        }
    }
}

impl Registry {
    /// 이 세션의 Claude 에 붙은 IDE 연결 번호들.
    pub fn ide_clients(&self, key: &str) -> Vec<u64> {
        let Some(e) = self.entries.get(key) else { return Vec::new() };
        self.ide_pids.iter().filter(|(_, &pid)| pid == e.found.pid).map(|(&id, _)| id).collect()
    }

    fn wt_json(&self, root: &Path) -> Value {
        let w = self.wt.get(root);
        let branch = w.map(|w| w.branch.as_str()).unwrap_or("");
        json!({
            "root": root,
            "name": root.file_name().map(|n| n.to_string_lossy().into_owned()),
            "branch": branch,
            "issue": issue_number(root, branch),
            "dirty": w.map(|w| w.dirty).unwrap_or(0),
        })
    }

    /// 화면에 보낼 세션 목록. 최근 활동 순.
    pub fn snapshot(&self) -> Value {
        let mut rows: Vec<(i64, Value)> = Vec::new();
        for (key, e) in &self.entries {
            let f = &e.found;
            let t = e.tracker.as_ref();
            let last = t.map(|t| t.last_epoch).unwrap_or(0);
            let uncommitted = e.commits.values().filter(|c| matches!(c, CommitState::No)).count();
            let s = f.file.as_ref();
            rows.push((
                last,
                json!({
                    "key": key,
                    "sessionId": s.map(|s| &s.session_id),
                    "name": s.map(|s| &s.name),
                    "status": s.map(|s| &s.status),
                    "version": s.map(|s| &s.version),
                    "pid": f.pid,
                    "cwd": e.cwd(),
                    "match": f.how.as_str(),
                    "jsonl": f.jsonl,
                    "compacts": t.map(|t| t.chain().len()).unwrap_or(0).saturating_sub(1),
                    "edits": t.map(|t| t.entries.len()).unwrap_or(0),
                    "uncommitted": uncommitted,
                    "lastActivity": last,
                    "ide": !self.ide_clients(key).is_empty(),
                    "pane": {
                        "id": f.pane.id,
                        "session": f.pane.session,
                        "windowId": f.pane.window_id,
                        "windowIndex": f.pane.window_index,
                        "windowName": f.pane.window_name,
                        "agent": f.pane.agent,
                        "agentStatus": f.pane.agent_status,
                        "width": f.pane.width,
                        "height": f.pane.height,
                    },
                    "worktree": e.primary().map(|r| self.wt_json(&r)),
                    "worktrees": e.roots().iter().map(|r| self.wt_json(r)).collect::<Vec<_>>(),
                    "parked": f.parked.as_ref().map(|p| json!({
                        "sessionId": p.session_id, "name": p.name, "status": p.status, "pid": p.pid,
                    })),
                }),
            ));
        }
        rows.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        Value::Array(rows.into_iter().map(|(_, v)| v).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_from_dir_or_branch() {
        assert_eq!(issue_number(Path::new("/r/myrepo-gh123"), "feature/x"), Some(123));
        assert_eq!(issue_number(Path::new("/r/myrepo"), "feat/GH-45-login"), Some(45));
        assert_eq!(issue_number(Path::new("/r/myrepo"), "fix/gh77"), Some(77));
        assert_eq!(issue_number(Path::new("/r/myrepo"), "develop"), None);
    }
}
