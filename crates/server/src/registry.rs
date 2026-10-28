//! 세션 목록 상태. 갱신 스레드 하나가 소유하고, 결과는 JSON 스냅숏으로 내보낸다(API·WebSocket 은 스냅숏만 읽는다).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

use ccdiff_core::{session::git_root, tracker::SessionTracker};

use crate::{
    discover::{self, Found},
    procs::Procs,
    tmux::Tmux,
};

const GIT_REFRESH: Duration = Duration::from_secs(10);

struct Entry {
    found: Found,
    tracker: Option<SessionTracker>,
}

/// 워크트리 하나의 git 정보. 10초마다 다시 읽는다.
#[derive(Clone)]
struct WtInfo {
    branch: String,
    /// `git status --porcelain` 줄 수(untracked 폴더는 한 줄).
    dirty: usize,
    at: Instant,
}

#[derive(Default)]
pub struct Registry {
    tmux: Tmux,
    entries: HashMap<String, Entry>,
    wt: HashMap<PathBuf, WtInfo>,
}

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

impl Registry {
    pub fn new(tmux: Tmux) -> Self {
        Self { tmux, ..Default::default() }
    }

    /// pane 을 다시 훑고, 각 세션 기록을 이어 읽는다.
    pub fn refresh(&mut self) {
        let panes = self.tmux.panes();
        let procs = Procs::snapshot();
        let found = discover::find(&panes, &procs);
        let keys: Vec<String> = found.iter().map(Found::key).collect();
        self.entries.retain(|k, _| keys.contains(k));
        for f in found {
            let key = f.key();
            let same_log = self.entries.get(&key).is_some_and(|e| e.found.jsonl == f.jsonl);
            if !same_log {
                // 처음 보거나 기록 파일이 바뀌었다(추정이 다른 파일로 옮겨 감) — 새로 읽는다.
                let tracker = f.jsonl.clone().and_then(|p| {
                    let mut t = SessionTracker::new(p);
                    t.load().ok().map(|_| t)
                });
                self.entries.insert(key, Entry { found: f, tracker });
                continue;
            }
            let e = self.entries.get_mut(&key).expect("entry");
            e.found = f;
            if let Some(t) = e.tracker.as_mut() {
                t.maybe_discover();
                let _ = t.ingest();
                t.take_changes();
            }
        }
    }

    fn wt_info(&mut self, root: &Path) -> WtInfo {
        if let Some(w) = self.wt.get(root).filter(|w| w.at.elapsed() < GIT_REFRESH) {
            return w.clone();
        }
        let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_default();
        let dirty = git(root, &["status", "--porcelain"]).map(|s| s.lines().count()).unwrap_or(0);
        let w = WtInfo { branch, dirty, at: Instant::now() };
        self.wt.insert(root.to_path_buf(), w.clone());
        w
    }

    fn wt_json(&mut self, root: &Path) -> Value {
        let w = self.wt_info(root);
        json!({
            "root": root,
            "name": root.file_name().map(|n| n.to_string_lossy().into_owned()),
            "branch": w.branch,
            "issue": issue_number(root, &w.branch),
            "dirty": w.dirty,
        })
    }

    /// 화면에 보낼 세션 목록. 최근 활동 순.
    pub fn snapshot(&mut self) -> Value {
        let mut rows: Vec<(i64, Value)> = Vec::new();
        let keys: Vec<String> = self.entries.keys().cloned().collect();
        for key in keys {
            let e = &self.entries[&key];
            let f = e.found.clone();
            let t = e.tracker.as_ref();
            // 대표 워크트리: 마지막으로 고친 파일의 워크트리, 없으면 마지막으로 다룬 워크트리, 없으면 cwd 의 git 루트.
            let cwd = f.file.as_ref().map(|s| s.cwd.clone()).unwrap_or_else(|| f.pane.path.clone());
            let primary = t
                .and_then(|t| t.entries.iter().rev().find_map(|x| x.root.clone()).or_else(|| t.wt_roots.last().cloned()))
                .or_else(|| git_root(&cwd));
            let roots: Vec<PathBuf> = t.map(|t| t.wt_roots.clone()).unwrap_or_default();
            let last = t.map(|t| t.last_epoch).unwrap_or(0);
            let edits = t.map(|t| t.entries.len()).unwrap_or(0);
            let chain = t.map(|t| t.chain().len()).unwrap_or(0);
            let worktree = primary.as_deref().map(|r| self.wt_json(r));
            let worktrees: Vec<Value> = roots.iter().map(|r| self.wt_json(r)).collect();
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
                    "cwd": cwd,
                    "match": f.how.as_str(),
                    "jsonl": f.jsonl,
                    "compacts": chain.saturating_sub(1),
                    "edits": edits,
                    "lastActivity": last,
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
                    "worktree": worktree,
                    "worktrees": worktrees,
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
