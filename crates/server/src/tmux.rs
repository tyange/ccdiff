//! tmux 읽기와 조작. WORKTABLE_TMUX_SOCKET 이 있으면 `tmux -L <이름>` 으로 그 서버만 본다(테스트용).

use std::{collections::HashSet, env, path::PathBuf, process::Command};

#[derive(Clone, Debug)]
pub struct Pane {
    pub id: String,
    pub pid: u32,
    pub session: String,
    pub window_id: String,
    pub window_index: u32,
    pub window_name: String,
    /// pane 에서 지금 도는 명령. Claude 는 프로세스 이름이 버전 문자열(예: 2.1.312)로 보인다.
    pub command: String,
    pub path: PathBuf,
    /// tmux-orch.sh 가 붙이는 pane 옵션.
    pub agent: String,
    pub agent_status: String,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Default)]
pub struct Tmux {
    socket: Option<String>,
}

const FIELDS: &[&str] = &[
    "pane_id",
    "pane_pid",
    "session_name",
    "window_id",
    "window_index",
    "window_name",
    "pane_current_command",
    "pane_current_path",
    "@agent",
    "@agent_status",
    "pane_width",
    "pane_height",
];

impl Tmux {
    pub fn from_env() -> Self {
        Self { socket: env::var("WORKTABLE_TMUX_SOCKET").ok().filter(|s| !s.is_empty()) }
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new("tmux");
        if let Some(s) = &self.socket {
            c.args(["-L", s]);
        }
        c
    }

    /// 성공하면 표준 출력. tmux 서버가 없으면 None.
    pub fn run(&self, args: &[&str]) -> Option<String> {
        let out = self.command().args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// 모든 pane. 웹 칸이 붙으려고 만든 보기용 세션은 원래 세션과 window 를 같이 쓰므로 빼고, pane 은 한 번씩만.
    pub fn panes(&self) -> Vec<Pane> {
        let fmt = FIELDS.iter().map(|f| format!("#{{{f}}}")).collect::<Vec<_>>().join("\t");
        let Some(raw) = self.run(&["list-panes", "-a", "-F", &fmt]) else { return Vec::new() };
        let mut seen = HashSet::new();
        raw.lines()
            .filter_map(parse_pane)
            .filter(|p| !is_view_session(&p.session) && seen.insert(p.id.clone()))
            .collect()
    }

    pub fn pane(&self, id: &str) -> Option<Pane> {
        self.panes().into_iter().find(|p| p.id == id)
    }

    pub fn has_session(&self, name: &str) -> bool {
        self.run(&["has-session", "-t", &format!("={name}")]).is_some()
    }

    /// window 안 pane 수.
    pub fn window_panes(&self, window_id: &str) -> usize {
        self.run(&["list-panes", "-t", window_id, "-F", "#{pane_id}"]).map(|s| s.lines().count()).unwrap_or(0)
    }

    /// 붙은 클라이언트가 없는 보기용 세션을 치운다(앞서 죽은 서버가 남긴 것).
    pub fn sweep_views(&self) {
        let Some(raw) = self.run(&["list-sessions", "-F", "#{session_name}\t#{session_attached}"]) else { return };
        for line in raw.lines() {
            if let Some((name, "0")) = line.split_once('\t') {
                if is_view_session(name) {
                    self.run(&["kill-session", "-t", &format!("={name}")]);
                }
            }
        }
    }
}

/// 웹 터미널 칸이 붙으려고 만든 보기용 세션 이름의 머리.
pub const VIEW_PREFIX: &str = "wt-view-";

/// 앱이 가져온·새로 띄운 Claude 를 두는 세션.
pub const HOME_SESSION: &str = "worktable";

pub fn is_view_session(name: &str) -> bool {
    name.starts_with(VIEW_PREFIX)
}

fn parse_pane(line: &str) -> Option<Pane> {
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() != FIELDS.len() {
        return None;
    }
    Some(Pane {
        id: f[0].into(),
        pid: f[1].parse().ok()?,
        session: f[2].into(),
        window_id: f[3].into(),
        window_index: f[4].parse().ok()?,
        window_name: f[5].into(),
        command: f[6].into(),
        path: PathBuf::from(f[7]),
        agent: f[8].into(),
        agent_status: f[9].into(),
        width: f[10].parse().unwrap_or(0),
        height: f[11].parse().unwrap_or(0),
    })
}
