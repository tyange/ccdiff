//! 서버 칸: dev 서버를 워크트리·포트로 짝지어 보이고, pane 서버는 로그를 보이고 확인을 받아 재시작한다.
//! - pane 서버: `@server` 표시가 있거나(tmux-orch.sh) `servers` window(WORKTABLE_SERVERS_WINDOW)에 있는 pane.
//!   로그는 capture-pane 으로 읽어 시크릿을 가린 뒤 보낸다.
//! - pane 밖 서버: 세션들이 다룬 레포의 워크트리에서 포트를 연 프로세스(Claude 의 Bash 가 띄운 것 등).
//!   로그가 없고(출력이 버려졌다) 남의 것이라 재시작하지 않는다.
//!
//! 재시작은 pane 에서 돌던 명령만 다시 친다 — 돌지 않던 서버를 새로 띄우지 않는다.

use std::{
    collections::{HashMap, HashSet},
    env,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

use ccdiff_core::session::git_root;

use crate::{
    actions::Nonces,
    claude::looks_like_claude,
    procs::Procs,
    registry::{issue_number, Registry},
    tmux::{Pane, Tmux},
};

const SHELLS: &[&str] = &["zsh", "bash", "sh", "fish"];

fn servers_window() -> String {
    env::var("WORKTABLE_SERVERS_WINDOW").unwrap_or_else(|_| "servers".into())
}

pub fn is_server_pane(p: &Pane) -> bool {
    p.agent.is_empty() && (!p.server.is_empty() || p.window_name == servers_window())
}

/// 레지스트리에서 뽑은 것 — lsof·git 은 잠금 밖에서 부르려고 먼저 복사한다.
pub struct Known {
    roots: Vec<PathBuf>,
    /// Claude pid → 세션 이름(목록에 보이는 것).
    claude: HashMap<u32, String>,
}

impl Known {
    pub fn from(reg: &Registry) -> Self {
        let mut roots = Vec::new();
        let mut claude = HashMap::new();
        for e in reg.entries.values() {
            roots.extend(e.roots());
            let f = &e.found;
            let label = if f.pane.agent.is_empty() { f.pane.window_name.clone() } else { f.pane.agent.clone() };
            if let Some(p) = &f.parked {
                claude.insert(p.pid, format!("{label}(백그라운드)"));
            }
            claude.insert(f.pid, label);
        }
        Self { roots, claude }
    }
}

/// 포트를 연 프로세스 → 포트들.
fn listening() -> HashMap<u32, Vec<u16>> {
    let mut out: HashMap<u32, Vec<u16>> = HashMap::new();
    let Ok(o) = Command::new("lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"]).output() else { return out };
    let mut pid = 0;
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        if let Some(p) = l.strip_prefix('p') {
            pid = p.parse().unwrap_or(0);
        } else if let Some(n) = l.strip_prefix('n') {
            if let Some(port) = n.rsplit(':').next().and_then(|x| x.parse().ok()) {
                let ports = out.entry(pid).or_default();
                if !ports.contains(&port) {
                    ports.push(port);
                }
            }
        }
    }
    out
}

/// 프로세스들의 cwd(한 번의 lsof).
fn cwds(pids: &[u32]) -> HashMap<u32, PathBuf> {
    let mut out = HashMap::new();
    if pids.is_empty() {
        return out;
    }
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let Ok(o) = Command::new("lsof").args(["-a", "-d", "cwd", "-Fpn", "-p", &list]).output() else { return out };
    let mut pid = 0;
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        if let Some(p) = l.strip_prefix('p') {
            pid = p.parse().unwrap_or(0);
        } else if let Some(n) = l.strip_prefix('n') {
            out.insert(pid, PathBuf::from(n));
        }
    }
    out
}

/// 세션들이 다룬 레포의 모든 워크트리 루트.
fn repo_worktrees(roots: &[PathBuf]) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    let mut seen = HashSet::new();
    for r in roots {
        if !seen.insert(r.clone()) {
            continue;
        }
        let Ok(o) = Command::new("git").arg("-C").arg(r).args(["worktree", "list", "--porcelain"]).output() else { continue };
        for l in String::from_utf8_lossy(&o.stdout).lines() {
            if let Some(p) = l.strip_prefix("worktree ") {
                out.insert(PathBuf::from(p));
                seen.insert(PathBuf::from(p));
            }
        }
    }
    out
}

/// Claude·ccdiff·worktable 자신이 연 포트(IDE 등)는 dev 서버가 아니다.
fn is_tool(command: &str) -> bool {
    let argv0 = command.split_whitespace().next().unwrap_or("");
    let name = argv0.rsplit('/').next().unwrap_or(argv0);
    looks_like_claude(command) || matches!(name, "ccdiff" | "worktable") || name.chars().all(|c| c.is_ascii_digit() || c == '.')
}

fn wt_json(root: Option<&Path>) -> Value {
    match root {
        Some(r) => json!({
            "root": r,
            "name": r.file_name().map(|n| n.to_string_lossy().into_owned()),
            "issue": issue_number(r, ""),
        }),
        None => Value::Null,
    }
}

/// ps 는 사람이 친 명령이 아니라 실행된 경로를 보인다(`/…/pnpm/global/…/pnpm --filter …`).
/// 그 이름이 PATH 에 있으면 이름으로 줄인다(pnpm 처럼 PATH 의 것이 실제 파일을 부르는 shim 인 경우가 많다).
/// 다시 칠 명령은 재시작 확인 창에 그대로 보인다.
fn shorten(command: &str) -> String {
    let (argv0, rest) = command.split_once(' ').unwrap_or((command, ""));
    let Some(name) = argv0.rsplit('/').next().filter(|n| argv0.contains('/') && !n.is_empty()) else { return command.into() };
    let on_path = env::var_os("PATH").is_some_and(|p| env::split_paths(&p).any(|d| d.join(name).is_file()));
    if on_path { format!("{name} {rest}").trim_end().to_string() } else { command.into() }
}

/// 돌던 명령을 기억해 둔다 — 멈춘 뒤에도 같은 명령으로 다시 띄울 수 있게. pane ID → 명령.
#[derive(Default)]
pub struct Remembered(Mutex<HashMap<String, String>>);

pub fn list(tmux: &Tmux, known: &Known, remembered: &Remembered) -> Vec<Value> {
    let panes: Vec<Pane> = tmux.panes().into_iter().filter(is_server_pane).collect();
    let procs = Procs::snapshot();
    let listen = listening();
    let me = std::process::id();
    let mut in_panes = HashSet::new();
    let mut rows = Vec::new();
    let mut mem = remembered.0.lock().ok();

    let pane_desc: Vec<Vec<u32>> = panes.iter().map(|p| procs.descendants(p.pid)).collect();
    let pids: Vec<u32> = listen.keys().copied().collect();
    let cwd = cwds(&pids);
    for (p, desc) in panes.iter().zip(&pane_desc) {
        in_panes.extend(desc.iter().copied());
        let child = procs.children(p.pid).first().copied();
        let command = child.and_then(|c| procs.command(c)).map(shorten).unwrap_or_default();
        if let (Some(m), false) = (mem.as_mut(), command.is_empty()) {
            m.insert(p.id.clone(), command.clone());
        }
        let ports: Vec<u16> = desc.iter().filter_map(|d| listen.get(d)).flatten().copied().collect();
        let listen_cwd = desc.iter().find(|d| listen.contains_key(d)).and_then(|d| cwd.get(d));
        let root = listen_cwd.and_then(|c| git_root(c)).or_else(|| git_root(&p.path));
        let last = mem.as_ref().and_then(|m| m.get(&p.id).cloned());
        rows.push(json!({
            "id": p.id,
            "kind": "pane",
            "name": if p.server.is_empty() { format!("{}.{}", p.window_name, p.id) } else { p.server.clone() },
            "pane": {"id": p.id, "session": p.session, "window": p.window_name},
            "running": child.is_some(),
            "command": if command.is_empty() { last.clone().unwrap_or_default() } else { command },
            "ports": ports,
            "worktree": wt_json(root.as_deref()),
            "owner": Value::Null,
            "restartable": child.is_some() || last.is_some(),
        }));
    }

    let worktrees = repo_worktrees(&known.roots);
    let mut detached: Vec<(u32, Value)> = Vec::new();
    for (&pid, ports) in &listen {
        if pid == me || in_panes.contains(&pid) {
            continue;
        }
        let command = procs.command(pid).unwrap_or("").to_string();
        if is_tool(&command) {
            continue;
        }
        let Some(root) = cwd.get(&pid).and_then(|c| git_root(c)).filter(|r| worktrees.contains(r)) else { continue };
        let ancestors = procs.ancestors(pid);
        let owner = ancestors
            .iter()
            .find_map(|a| known.claude.get(a).cloned())
            .or_else(|| ancestors.iter().find(|&&a| procs.command(a).is_some_and(looks_like_claude)).map(|a| format!("Claude(pid {a})")));
        let mut ports = ports.clone();
        ports.sort_unstable();
        detached.push((
            pid,
            json!({
                "id": format!("pid:{pid}"),
                "kind": "detached",
                "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                "pane": Value::Null,
                "running": true,
                "command": command,
                "ports": ports,
                "worktree": wt_json(Some(&root)),
                "owner": owner,
                "restartable": false,
            }),
        ));
    }
    detached.sort_by_key(|(_, v)| v["ports"][0].as_u64());
    rows.extend(detached.into_iter().map(|(_, v)| v));
    rows
}

/// pane 로그(최근 1000줄, 감긴 줄은 이어 붙임). 시크릿은 가린다.
pub fn log(tmux: &Tmux, pane: &str) -> Option<String> {
    tmux.run(&["capture-pane", "-p", "-J", "-S", "-1000", "-t", pane])
        .map(|t| ccdiff_core::secret::mask_secrets(t.trim_end()))
}

pub struct RestartPlan {
    pane: String,
    command: String,
}

pub type RestartPlans = Nonces<RestartPlan>;

/// 재시작 계획: 무엇을 멈추고 무엇을 다시 치는지와 nonce.
pub fn plan_restart(tmux: &Tmux, known: &Known, remembered: &Remembered, plans: &RestartPlans, pane: &str) -> Result<Value, String> {
    let row = list(tmux, known, remembered)
        .into_iter()
        .find(|r| r["id"] == pane && r["kind"] == "pane")
        .ok_or("서버 pane 이 아니다")?;
    let command = row["command"].as_str().unwrap_or_default().to_string();
    if command.is_empty() {
        return Err("이 pane 에서 돌던 명령을 모른다 — 직접 띄워야 한다".into());
    }
    let running = row["running"].as_bool().unwrap_or(false);
    let mut warnings = Vec::new();
    if running {
        warnings.push(format!("pane {pane} ({}) 에 C-c 를 보내 멈춘다.", row["name"].as_str().unwrap_or("")));
    } else {
        warnings.push(format!("pane {pane} 은 멈춰 있다 — 마지막으로 돌던 명령을 다시 친다."));
    }
    warnings.push(format!("그 pane 의 셸에 다시 칠 명령: {command}"));
    if let Some(ports) = row["ports"].as_array().filter(|p| !p.is_empty()) {
        let ports: Vec<String> = ports.iter().map(|p| p.to_string()).collect();
        warnings.push(format!("지금 쓰는 포트: {}", ports.join(", ")));
    }
    warnings.push("다른 Claude 세션이 이 서버를 쓰고 있으면 잠깐 끊긴다.".into());
    let nonce = plans.put(RestartPlan { pane: pane.to_string(), command: command.clone() }).ok_or("nonce 를 만들지 못했다")?;
    Ok(json!({"pane": pane, "command": command, "warnings": warnings, "nonce": nonce}))
}

/// C-c 로 멈추고 셸로 돌아오면(최대 15초) 같은 명령을 다시 친다.
pub fn restart(tmux: &Tmux, plans: &RestartPlans, nonce: &str) -> Result<Value, String> {
    let plan = plans.take(nonce).ok_or("확인이 만료됐거나 이미 썼다 — 다시 시도")?;
    let at_shell = || {
        tmux.run(&["display", "-p", "-t", &plan.pane, "#{pane_current_command}"])
            .is_some_and(|c| SHELLS.contains(&c.trim()))
    };
    // C-c 는 처음에 한 번, 5초 지나도 안 멈추면 한 번 더 — 종료 정리 중인 서버를 거듭 끊지 않게.
    let start = Instant::now();
    let mut sent = 0;
    while !at_shell() {
        let waited = start.elapsed();
        if waited > Duration::from_secs(15) {
            return Err("15초 안에 멈추지 않았다 — pane 을 직접 확인".into());
        }
        if sent == 0 || (sent == 1 && waited > Duration::from_secs(5)) {
            tmux.run(&["send-keys", "-t", &plan.pane, "C-c"]);
            sent += 1;
        }
        thread::sleep(Duration::from_millis(300));
    }
    tmux.run(&["send-keys", "-t", &plan.pane, "-l", &plan.command]).ok_or("명령을 보내지 못했다")?;
    tmux.run(&["send-keys", "-t", &plan.pane, "Enter"]);
    Ok(json!({"pane": plan.pane, "command": plan.command}))
}
