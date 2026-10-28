//! 세션의 수정이 커밋에 들어갔는지 판정한다.
//!
//! 수정 시각 이후 그 워크트리 HEAD 에서 그 파일을 건드린 첫 커밋을 찾고, 그 커밋의 파일 내용에 수정으로 추가된
//! 줄이 전부 들어 있으면 "커밋됨" 으로 본다. 시각만 보면 되돌린 수정까지 커밋됨으로 잡힌다.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use crate::session::EditEntry;

const REFRESH: Duration = Duration::from_secs(10);
/// 내용 대조에 쓰는 추가 줄 수 상한(큰 새 파일에서 git show 결과 전체를 훑지 않게).
const MAX_PROBE_LINES: usize = 200;

#[derive(Clone)]
pub enum CommitState {
    /// git 을 읽지 못함(워크트리가 지워졌거나 git 밖).
    Unknown,
    No,
    Yes { hash: String, subject: String },
    /// 이후 커밋에 파일은 들어갔지만 이 수정의 추가 줄이 그대로 남지 않았다(커밋 전에 더 고침).
    Changed { hash: String, subject: String },
    /// 커밋된 적 없고 지금 파일도 없다(임시 파일 등).
    Gone,
}

struct Commit {
    hash: String,
    time: i64,
    subject: String,
    files: HashSet<String>,
}

struct RepoLog {
    at: Instant,
    commits: Vec<Commit>,
}

#[derive(Default)]
pub struct GitIndex {
    logs: HashMap<PathBuf, RepoLog>,
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn read_log(root: &Path, since: i64) -> Option<Vec<Commit>> {
    // 그 워크트리 브랜치(HEAD)에서 닿는 커밋만 본다. --all 이면 다른 브랜치가 같은 파일을 고친 커밋을 잘못 잡는다.
    let since = format!("--since={since}");
    let raw = git(root, &["log", "HEAD", &since, "--name-only", "--format=%x00%H%x09%ct%x09%s"])?;
    let mut commits = Vec::new();
    for chunk in raw.split('\0').filter(|c| !c.trim().is_empty()) {
        let mut lines = chunk.lines();
        let mut head = lines.next()?.splitn(3, '\t');
        let hash = head.next()?.to_string();
        let time = head.next()?.parse().ok()?;
        let subject = head.next().unwrap_or("").to_string();
        let files = lines.filter(|l| !l.is_empty()).map(String::from).collect();
        commits.push(Commit { hash, time, subject, files });
    }
    commits.sort_by_key(|c| c.time);
    Some(commits)
}

fn added_lines(e: &EditEntry) -> Vec<String> {
    let mut out: Vec<String> = if e.hunks.is_empty() {
        e.content.as_deref().unwrap_or("").lines().map(String::from).collect()
    } else {
        e.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter_map(|l| l.strip_prefix('+').map(String::from))
            .collect()
    };
    out.retain(|l| !l.trim().is_empty());
    out.truncate(MAX_PROBE_LINES);
    out
}

impl GitIndex {
    /// 오래된 로그를 버린다. 다음 판정 때 다시 읽는다.
    pub fn expire(&mut self) -> bool {
        let before = self.logs.len();
        self.logs.retain(|_, l| l.at.elapsed() < REFRESH);
        self.logs.len() != before
    }

    pub fn state(&mut self, e: &EditEntry, since: i64) -> CommitState {
        let Some(root) = &e.root else { return CommitState::Unknown };
        if !self.logs.contains_key(root) {
            match read_log(root, since) {
                Some(commits) => {
                    self.logs.insert(root.clone(), RepoLog { at: Instant::now(), commits });
                }
                None => return CommitState::Unknown,
            }
        }
        let rel = e.rel_path();
        let log = &self.logs[root];
        let Some(c) = log.commits.iter().find(|c| c.time >= e.epoch && c.files.contains(&rel)) else {
            return if e.path.exists() { CommitState::No } else { CommitState::Gone };
        };
        let hash = c.hash[..7.min(c.hash.len())].to_string();
        let subject = c.subject.clone();
        let probe = added_lines(e);
        if !probe.is_empty() {
            let blob = git(root, &["show", &format!("{}:{}", c.hash, rel)]).unwrap_or_default();
            let have: HashSet<&str> = blob.lines().map(str::trim_end).collect();
            if !probe.iter().all(|l| have.contains(l.trim_end())) {
                return CommitState::Changed { hash, subject };
            }
        }
        CommitState::Yes { hash, subject }
    }
}
