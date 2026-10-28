//! Claude Code 가 남기는 파일 읽기. 모두 공개 규격이 아닌 내부 형식이다 — `worktable doctor` 로 짝짓기 근거를 확인한다.
//! - `sessions/<pid>.json`: 실행 중인 Claude 마다 하나. sessionId·cwd·status, tmux 안이면 `tmux: "세션:@창.%pane"`.
//!   `--resume <ID>` 로 띄워도 실제 기록 ID 는 달라질 수 있어 argv 보다 이 파일을 믿는다(2.1.310·2.1.312 실측).
//! - `projects/<cwd 인코딩>/<sessionId>.jsonl`: 세션 기록.

use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use ccdiff_core::session::project_dir_name;

/// CLAUDE_CONFIG_DIR 이 있으면 그것, 없으면 ~/.claude.
pub fn config_dir() -> PathBuf {
    match env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".claude"),
    }
}

#[derive(Clone, Debug)]
pub struct SessionFile {
    pub pid: u32,
    pub session_id: String,
    pub cwd: PathBuf,
    /// idle·busy·shell 등.
    pub status: String,
    pub name: String,
    /// interactive·bg 등.
    pub kind: String,
    pub version: String,
    /// `세션:@창.%pane` — tmux 안에서 띄웠을 때만.
    pub tmux: Option<String>,
    /// 이 세션을 백그라운드 작업으로 넘겼으면 그 작업 ID.
    pub parked_job: Option<String>,
    /// 백그라운드 작업이면 자기 작업 ID.
    pub job_id: Option<String>,
    /// 프로세스 시작 시각(UTC). pid 재사용을 가려내는 데 쓴다.
    pub proc_start: Option<String>,
}

impl SessionFile {
    /// `tmux` 필드의 pane ID(`%N`).
    pub fn tmux_pane(&self) -> Option<&str> {
        self.tmux.as_deref().and_then(|t| t.rsplit_once('.')).map(|(_, p)| p)
    }
}

fn parse(v: &Value) -> Option<SessionFile> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    Some(SessionFile {
        pid: v.get("pid")?.as_u64()? as u32,
        session_id: s("sessionId")?,
        cwd: PathBuf::from(s("cwd").unwrap_or_default()),
        status: s("status").unwrap_or_default(),
        name: s("name").unwrap_or_default(),
        kind: s("kind").unwrap_or_default(),
        version: s("version").unwrap_or_default(),
        tmux: s("tmux"),
        parked_job: s("parkedJobId"),
        job_id: s("jobId"),
        proc_start: s("procStart"),
    })
}

pub fn session_file(pid: u32) -> Option<SessionFile> {
    let text = fs::read_to_string(config_dir().join("sessions").join(format!("{pid}.json"))).ok()?;
    parse(&serde_json::from_str(&text).ok()?)
}

/// 모든 세션 파일. 죽은 프로세스의 파일도 남아 있으니 쓰는 쪽이 살아 있는지 본다.
pub fn all_session_files() -> Vec<SessionFile> {
    let dir = config_dir().join("sessions");
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .filter_map(|v| parse(&v))
        .collect()
}

/// 세션 기록 파일. cwd 프로젝트 폴더에 없으면 모든 프로젝트 폴더에서 찾는다.
pub fn find_jsonl(session_id: &str, cwd: &Path) -> Option<PathBuf> {
    let projects = config_dir().join("projects");
    let name = format!("{session_id}.jsonl");
    let direct = projects.join(project_dir_name(cwd)).join(&name);
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(&projects).ok()?.flatten().map(|d| d.path().join(&name)).find(|p| p.is_file())
}

/// 세션 파일이 없을 때의 추정: cwd 프로젝트 폴더에서 가장 최근에 쓴 기록.
/// claimed 는 살아 있는 다른 Claude 가 쓰는 세션 ID — 그 기록은 고르지 않는다.
pub fn newest_jsonl(cwd: &Path, claimed: &HashSet<String>) -> Option<PathBuf> {
    fs::read_dir(config_dir().join("projects").join(project_dir_name(cwd)))
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter(|e| !e.path().file_stem().is_some_and(|s| claimed.contains(&*s.to_string_lossy())))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// Claude 로 보이는 명령인가 — 세션 파일이 없을 때만 쓰는 약한 판정.
pub fn looks_like_claude(command: &str) -> bool {
    let argv0 = command.split_whitespace().next().unwrap_or("");
    argv0 == "claude" || argv0.ends_with("/claude") || argv0.contains("/claude/versions/")
}
