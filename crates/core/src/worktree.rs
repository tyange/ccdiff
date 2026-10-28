//! 워크트리의 `git diff HEAD` 와 untracked 파일을 수정 항목으로 만든다.
//! 세션 기록에 diff 가 남지 않는 Bash 수정(sed·스크립트 치환 등)도 여기서 잡힌다.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use crate::{
    secret::is_secret,
    session::{fmt_epoch, EditEntry, Hunk},
};

const MAX_UNTRACKED: usize = 30;
const MAX_FILE_BYTES: u64 = 256 * 1024;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn mtime(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn entry(root: &Path, rel: &str, kind: &'static str, hunks: Vec<Hunk>, before: String) -> EditEntry {
    let path = root.join(rel);
    let (mut added, mut removed) = (0, 0);
    for l in hunks.iter().flat_map(|h| &h.lines) {
        match l.chars().next() {
            Some('+') => added += 1,
            Some('-') => removed += 1,
            _ => {}
        }
    }
    let epoch = mtime(&path);
    EditEntry {
        time: fmt_epoch(epoch),
        epoch,
        kind,
        path,
        root: Some(root.to_path_buf()),
        hunks,
        before: Some(before),
        content: None,
        added,
        removed,
        user_modified: false,
    }
}

fn parse_range(s: &str) -> (usize, usize) {
    let mut it = s.splitn(2, ',');
    let start = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let len = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
    (start, len)
}

struct FileDiff {
    old: Option<String>,
    new: Option<String>,
    hunks: Vec<Hunk>,
    binary: bool,
}

fn parse_diff(raw: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    for line in raw.lines() {
        if line.starts_with("diff --git ") {
            files.push(FileDiff { old: None, new: None, hunks: Vec::new(), binary: false });
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if f.hunks.is_empty() {
            if let Some(p) = line.strip_prefix("--- ") {
                f.old = p.strip_prefix("a/").map(String::from);
                continue;
            }
            if let Some(p) = line.strip_prefix("+++ ") {
                f.new = p.strip_prefix("b/").map(String::from);
                continue;
            }
            if line.starts_with("Binary files ") {
                f.binary = true;
                continue;
            }
        }
        if let Some(rest) = line.strip_prefix("@@ -") {
            let mut parts = rest.split(' ');
            let (old_start, old_lines) = parse_range(parts.next().unwrap_or(""));
            let (new_start, _) = parse_range(parts.next().unwrap_or("").trim_start_matches('+'));
            f.hunks.push(Hunk { old_start, old_lines, new_start, lines: Vec::new() });
            continue;
        }
        if let Some(h) = f.hunks.last_mut() {
            if matches!(line.chars().next(), Some(' ' | '+' | '-' | '\\')) {
                h.lines.push(line.to_string());
            }
        }
    }
    files
}

pub fn entries(root: &Path) -> Vec<EditEntry> {
    let mut out = Vec::new();
    if let Some(raw) = git(root, &["diff", "HEAD", "--no-color", "--no-ext-diff", "--no-renames", "-U3"]) {
        for f in parse_diff(&raw) {
            let Some(rel) = f.new.clone().or_else(|| f.old.clone()) else { continue };
            if f.binary || is_secret(&rel) {
                continue;
            }
            let (kind, before) = match (&f.old, &f.new) {
                (None, _) => ("git+", String::new()),
                (Some(o), None) => ("git-", git(root, &["show", &format!("HEAD:{o}")]).unwrap_or_default()),
                (Some(o), Some(_)) => ("git", git(root, &["show", &format!("HEAD:{o}")]).unwrap_or_default()),
            };
            out.push(entry(root, &rel, kind, f.hunks, before));
        }
    }
    if let Some(raw) = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]) {
        for rel in raw.split('\0').filter(|r| !r.is_empty() && !is_secret(r)).take(MAX_UNTRACKED) {
            let path: PathBuf = root.join(rel);
            if fs::metadata(&path).map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else { continue };
            if bytes.contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<String> = text.lines().map(|l| format!("+{l}")).collect();
            let hunk = Hunk { old_start: 0, old_lines: 0, new_start: 1, lines };
            out.push(entry(root, rel, "git+", vec![hunk], String::new()));
        }
    }
    out
}
