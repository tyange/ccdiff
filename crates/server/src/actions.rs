//! tmux 배치를 바꾸는 동작: 다른 세션의 Claude 를 앱 세션(worktable)으로 가져오기, 워크트리에서 새 Claude 띄우기.
//! 가져오기는 남의 화면 배치를 바꾸므로 계획(경고 포함)을 먼저 보이고, 화면에서 확인한 일회용 nonce 로만 실행한다.

use std::{
    collections::{HashMap, HashSet},
    env,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

use ccdiff_core::secret::random_token;

use crate::{
    registry::issue_number,
    tmux::{Tmux, HOME_SESSION},
};

const PLAN_TTL: Duration = Duration::from_secs(120);

pub struct ImportPlan {
    pane: String,
    name: String,
}

#[derive(Default)]
pub struct Plans(Mutex<HashMap<String, (ImportPlan, Instant)>>);

impl Plans {
    fn put(&self, plan: ImportPlan) -> Option<String> {
        let nonce = random_token()?;
        let mut m = self.0.lock().ok()?;
        m.retain(|_, (_, at)| at.elapsed() < PLAN_TTL);
        m.insert(nonce.clone(), (plan, Instant::now()));
        Some(nonce)
    }

    /// 한 번만 쓸 수 있다.
    fn take(&self, nonce: &str) -> Option<ImportPlan> {
        let (plan, at) = self.0.lock().ok()?.remove(nonce)?;
        (at.elapsed() < PLAN_TTL).then_some(plan)
    }
}

/// 가져오기 계획: 무엇이 어떻게 바뀌는지와 nonce.
pub fn plan_import(tmux: &Tmux, plans: &Plans, pane_id: &str) -> Result<Value, String> {
    let pane = tmux.pane(pane_id).ok_or("pane 이 없다")?;
    if pane.session == HOME_SESSION {
        return Err("이미 worktable 세션에 있다".into());
    }
    let name = if pane.agent.is_empty() { pane.window_name.clone() } else { pane.agent.clone() };
    let mut warnings = vec![format!(
        "{}:{} {} 에서 이 pane 을 빼 worktable 세션의 window '{name}' 으로 옮긴다. Claude 프로세스는 그대로 돈다.",
        pane.session, pane.window_index, pane.window_name
    )];
    if tmux.window_panes(&pane.window_id) <= 1 {
        warnings.push(format!("{} 세션에서 window '{}' 이 사라진다.", pane.session, pane.window_name));
    } else {
        warnings.push(format!("window '{}' 의 다른 pane 은 {} 세션에 남는다.", pane.window_name, pane.session));
    }
    if !pane.agent.is_empty() {
        warnings.push(format!("@agent={} 표시는 pane 을 따라간다 — tmux-orch.sh 의 agent 명령은 계속 이 pane 을 찾는다.", pane.agent));
    }
    warnings.push(format!(
        "가져온 뒤에는 {} 세션에 붙은 터미널에서 보이지 않는다. `tmux attach -t {HOME_SESSION}` 으로 볼 수 있다.",
        pane.session
    ));
    let nonce = plans.put(ImportPlan { pane: pane.id.clone(), name: name.clone() }).ok_or("nonce 를 만들지 못했다")?;
    Ok(json!({"pane": pane.id, "from": pane.session, "window": name, "warnings": warnings, "nonce": nonce}))
}

/// 앱 세션이 없으면 만든다. 새로 만들었으면 그 첫 window ID(나중에 지울 자리 채움용).
fn ensure_home(tmux: &Tmux, dir: Option<&Path>) -> Result<Option<String>, String> {
    if tmux.has_session(HOME_SESSION) {
        return Ok(None);
    }
    let mut args = vec!["new-session", "-d", "-s", HOME_SESSION, "-P", "-F", "#{window_id}"];
    let d;
    if let Some(p) = dir {
        d = p.to_string_lossy().into_owned();
        args.extend(["-c", d.as_str()]);
    }
    tmux.run(&args).map(|w| Some(w.trim().to_string())).ok_or_else(|| "worktable 세션을 만들지 못했다".into())
}

pub fn import(tmux: &Tmux, plans: &Plans, nonce: &str) -> Result<Value, String> {
    let plan = plans.take(nonce).ok_or("확인이 만료됐거나 이미 썼다 — 다시 시도")?;
    let placeholder = ensure_home(tmux, None)?;
    let moved = tmux.run(&[
        "break-pane",
        "-d",
        "-s",
        &plan.pane,
        "-t",
        &format!("={HOME_SESSION}:"),
        "-n",
        &plan.name,
        "-P",
        "-F",
        "#{window_id}",
    ]);
    if let Some(w) = placeholder {
        // 세션을 만들 때 생긴 빈 셸 window. 옮기기에 실패했으면 세션째 지운다.
        match moved {
            Some(_) => tmux.run(&["kill-window", "-t", &w]),
            None => tmux.run(&["kill-session", "-t", &format!("={HOME_SESSION}")]),
        };
    }
    let window = moved.ok_or("break-pane 실패")?;
    Ok(json!({"pane": plan.pane, "window": window.trim()}))
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 세션들이 다룬 워크트리가 속한 레포의 모든 워크트리. `git worktree list --porcelain`.
pub fn worktrees(sessions: &Value) -> Vec<Value> {
    let roots: Vec<PathBuf> = sessions
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["worktrees"].as_array().into_iter().flatten().chain(s["worktree"].as_object().map(|_| &s["worktree"])))
        .filter_map(|w| w["root"].as_str().map(PathBuf::from))
        .collect();
    let mut repos = HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        let Some(common) = git(&root, &["rev-parse", "--path-format=absolute", "--git-common-dir"]) else { continue };
        if !repos.insert(common.trim().to_string()) {
            continue;
        }
        let Some(raw) = git(&root, &["worktree", "list", "--porcelain"]) else { continue };
        for block in raw.split("\n\n") {
            let mut path = None;
            let mut branch = String::new();
            for l in block.lines() {
                if let Some(p) = l.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(p));
                } else if let Some(b) = l.strip_prefix("branch ") {
                    branch = b.trim_start_matches("refs/heads/").to_string();
                }
            }
            let Some(path) = path.filter(|p| p.is_dir()) else { continue };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            out.push(json!({"path": path, "name": name, "branch": branch, "issue": issue_number(&path, &branch)}));
        }
    }
    out
}

/// 워크트리에서 새 Claude 를 띄운다. 셸에 명령을 쳐 넣어(tmux-orch.sh 와 같은 방식) 셸 설정·PATH 를 그대로 쓴다.
/// WORKTABLE_CLAUDE_CMD 로 명령을 바꿀 수 있다(테스트는 `claude --model haiku`).
pub fn new_session(tmux: &Tmux, sessions: &Value, path: &str) -> Result<Value, String> {
    let known = worktrees(sessions);
    let wt = known.iter().find(|w| w["path"].as_str() == Some(path)).ok_or("알려진 워크트리가 아니다")?;
    let name = match wt["issue"].as_u64() {
        Some(n) => format!("gh{n}"),
        None => wt["name"].as_str().unwrap_or("claude").to_string(),
    };
    let dir = Path::new(path);
    let pane = match ensure_home(tmux, Some(dir))? {
        Some(w) => {
            tmux.run(&["rename-window", "-t", &w, &name]);
            tmux.run(&["display", "-p", "-t", &w, "#{pane_id}"])
        }
        None => tmux.run(&["new-window", "-d", "-t", &format!("={HOME_SESSION}:"), "-n", &name, "-c", path, "-P", "-F", "#{pane_id}"]),
    }
    .map(|p| p.trim().to_string())
    .ok_or("window 를 만들지 못했다")?;
    let cmd = env::var("WORKTABLE_CLAUDE_CMD").unwrap_or_else(|_| "claude".into());
    tmux.run(&["send-keys", "-t", &pane, "-l", &cmd]);
    tmux.run(&["send-keys", "-t", &pane, "Enter"]);
    Ok(json!({"pane": pane, "window": name, "path": path}))
}
