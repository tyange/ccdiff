use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde_json::Value;

pub struct Hunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub lines: Vec<String>,
}

pub struct EditEntry {
    pub time: String,
    /// 수정 시각(유닉스 초). 커밋 판정에 쓴다.
    pub epoch: i64,
    pub kind: &'static str,
    pub path: PathBuf,
    pub root: Option<PathBuf>,
    pub hunks: Vec<Hunk>,
    /// 수정 전 파일 전체. 기록에 없으면 diff 조각만으로 그린다.
    pub before: Option<String>,
    /// Write 가 쓴 파일 전체(새 파일일 때 diff 대신 쓴다).
    pub content: Option<String>,
    pub added: usize,
    pub removed: usize,
    pub user_modified: bool,
}

impl EditEntry {
    pub fn rel_path(&self) -> String {
        match &self.root {
            Some(r) => self.path.strip_prefix(r).unwrap_or(&self.path).display().to_string(),
            None => self.path.display().to_string(),
        }
    }

    pub fn short_path(&self) -> String {
        match &self.root {
            Some(root) => {
                let name = root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                format!("{} · {}", name, self.rel_path())
            }
            None => self.rel_path(),
        }
    }

    /// 목록용 짧은 워크트리 이름: `abc-web-apps-gh2171` → `gh2171`, 메인 레포는 `main`.
    pub fn label(&self) -> String {
        let Some(root) = &self.root else { return "-".into() };
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(i) = name.rfind("-gh") {
            return name[i + 1..].to_string();
        }
        match name.rsplit('-').next() {
            Some(tail) if name.contains('-') && tail.chars().all(|c| c.is_ascii_digit()) => tail.to_string(),
            _ => "main".into(),
        }
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// 세션 JSONL 을 이어 읽는다. 쓰는 중인 마지막 줄은 다음 읽기까지 남겨 둔다.
pub struct Tail {
    pub path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    pub fn new(path: PathBuf) -> Self {
        Self { path, offset: 0, partial: Vec::new() }
    }

    pub fn read_lines(&mut self) -> Result<Vec<String>> {
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

pub fn git_root(dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

fn epoch(ts: Option<&str>) -> i64 {
    ts.and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|d| d.timestamp()).unwrap_or(0)
}

/// 기록 한 줄의 timestamp(유닉스 초). 없으면 0.
pub fn line_epoch(line: &str) -> i64 {
    let key = "\"timestamp\":\"";
    let Some(i) = line.find(key) else { return 0 };
    let rest = &line[i + key.len()..];
    let ts = rest.split('"').next();
    epoch(ts)
}

pub fn fmt_epoch(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0)
        .filter(|_| epoch > 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "--".into())
}

/// Bash 명령에 나온 절대경로들의 git 루트. Bash 로만 고친 워크트리를 찾는 데 쓴다.
pub fn bash_roots(line: &str, roots: &mut HashMap<PathBuf, Option<PathBuf>>) -> Vec<PathBuf> {
    if !line.contains("\"name\":\"Bash\"") {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else { return Vec::new() };
    let home = std::env::var("HOME").unwrap_or_default();
    let mut out = Vec::new();
    for c in content {
        if c.get("name").and_then(Value::as_str) != Some("Bash") {
            continue;
        }
        let Some(cmd) = c.pointer("/input/command").and_then(Value::as_str) else { continue };
        for tok in cmd.split(|ch: char| ch.is_whitespace() || ";|&()<>\"'=`$".contains(ch)) {
            let tok = match tok.strip_prefix("~/") {
                Some(rest) => format!("{home}/{rest}"),
                None if tok.starts_with('/') => tok.to_string(),
                None => continue,
            };
            let mut dir = PathBuf::from(tok);
            while !dir.is_dir() {
                if !dir.pop() {
                    break;
                }
            }
            if dir.as_os_str().is_empty() || dir == Path::new("/") {
                continue;
            }
            if let Some(root) = roots.entry(dir.clone()).or_insert_with(|| git_root(&dir)).clone() {
                if !out.contains(&root) {
                    out.push(root);
                }
            }
        }
    }
    out
}

fn local_time(ts: Option<&str>) -> String {
    ts.and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "--".into())
}

fn num(v: &Value, key: &str) -> usize {
    v.get(key).and_then(Value::as_u64).unwrap_or(0) as usize
}

pub fn parse_entry(line: &str, roots: &mut HashMap<PathBuf, Option<PathBuf>>) -> Option<EditEntry> {
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

    let hunks: Vec<Hunk> = r
        .get("structuredPatch")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|h| Hunk {
                    old_start: num(h, "oldStart"),
                    old_lines: num(h, "oldLines"),
                    new_start: num(h, "newStart"),
                    lines: h
                        .get("lines")
                        .and_then(Value::as_array)
                        .map(|ls| ls.iter().filter_map(|l| l.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();

    let content = r.get("content").and_then(Value::as_str).map(String::from);
    let (mut added, mut removed) = (0, 0);
    if hunks.is_empty() && kind == "Write+" {
        added = content.as_deref().map(|c| c.lines().count()).unwrap_or(0);
    }
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
        epoch: epoch(v.get("timestamp").and_then(Value::as_str)),
        kind,
        path,
        root,
        hunks,
        before: r.get("originalFile").and_then(Value::as_str).map(String::from),
        content,
        added,
        removed,
        user_modified: r.get("userModified").and_then(Value::as_bool).unwrap_or(false),
    })
}
