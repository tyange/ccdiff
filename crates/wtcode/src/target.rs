//! 투두를 보낼 Claude Code 세션(tmux pane). Claude 는 tmux 에서 명령 이름이 버전 문자열(예: 2.1.310)로 보인다.
//! 보낼 때는 bracketed paste(load-buffer + paste-buffer -p)로 넣는다 — 여러 줄도 한 번의 입력으로 들어간다.

use std::{
    env,
    io::Write,
    process::{Command, Stdio},
};

#[derive(Clone, Debug)]
pub struct ClaudePane {
    pub id: String,
    /// 목록·제목에 보이는 이름: `세션:창 이름` + agent 표시나 폴더 이름.
    pub label: String,
    window: String,
}

fn tmux(args: &[&str]) -> Option<String> {
    let out = Command::new("tmux").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn looks_like_claude(command: &str) -> bool {
    command == "claude" || (command.contains('.') && command.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// tmux 의 모든 Claude pane. 웹 앱(worktable)의 보기용 세션처럼 같은 pane 이 두 번 보이면 한 번만.
pub fn claude_panes() -> Vec<ClaudePane> {
    let fmt = "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t#{window_id}\t#{pane_current_command}\t#{@agent}\t#{pane_current_path}";
    let Some(raw) = tmux(&["list-panes", "-a", "-F", fmt]) else { return Vec::new() };
    let mut out: Vec<ClaudePane> = Vec::new();
    for l in raw.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() != 8 || f[1].starts_with("wt-view-") || out.iter().any(|p| p.id == f[0]) {
            continue;
        }
        if !looks_like_claude(f[5]) && f[6].is_empty() {
            continue;
        }
        let tail = if f[6].is_empty() { f[7].rsplit('/').next().unwrap_or("") } else { f[6] };
        out.push(ClaudePane { id: f[0].into(), label: format!("{}:{} {} · {tail}", f[1], f[2], f[3]), window: f[4].into() });
    }
    out
}

/// 기본 대상: wtcode 와 같은 window 의 Claude, 없으면 Claude 가 하나뿐일 때 그것.
pub fn default_target(panes: &[ClaudePane]) -> Option<ClaudePane> {
    let here = env::var("TMUX_PANE").ok().and_then(|p| tmux(&["display", "-p", "-t", &p, "#{window_id}"]));
    if let Some(w) = here.map(|w| w.trim().to_string()).filter(|w| !w.is_empty()) {
        if let Some(p) = panes.iter().find(|p| p.window == w) {
            return Some(p.clone());
        }
    }
    (panes.len() == 1).then(|| panes[0].clone())
}

/// pane 이 아직 있는가(Claude 가 끝났거나 pane 이 닫혔으면 다시 골라야 한다).
/// 없는 pane 을 물어도 tmux 3.8 의 display 는 종료 코드 0 에 빈 출력을 준다(실측) — 출력으로 판정한다.
pub fn alive(pane: &str) -> bool {
    tmux(&["display", "-p", "-t", pane, "#{pane_id}"]).is_some_and(|o| o.trim() == pane)
}

/// text 를 그 pane 의 입력에 붙여 넣는다. submit 이면 Enter 까지 친다.
pub fn send(pane: &str, text: &str, submit: bool) -> Result<(), String> {
    let buf = format!("wtcode-{}", std::process::id());
    let mut child = Command::new("tmux")
        .args(["load-buffer", "-b", &buf, "-"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("tmux 실행 실패: {e}"))?;
    child.stdin.take().ok_or("tmux 입력을 열지 못했다")?.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("tmux load-buffer 실패".into());
    }
    tmux(&["paste-buffer", "-p", "-d", "-b", &buf, "-t", pane]).ok_or("붙여 넣지 못했다 — 대상 pane 이 없다")?;
    if submit {
        // 붙여 넣기가 화면에 반영된 뒤에 보낸다(tmux-orch.sh 의 send_text 와 같은 간격).
        std::thread::sleep(std::time::Duration::from_millis(300));
        tmux(&["send-keys", "-t", pane, "Enter"]).ok_or("Enter 를 보내지 못했다")?;
    }
    Ok(())
}

/// macOS 클립보드에 넣는다(⌘C 는 터미널이 먼저 가로채 앱이 받을 수 없다).
pub fn copy(text: &str) -> Result<(), String> {
    let mut child = Command::new("pbcopy").stdin(Stdio::piped()).spawn().map_err(|e| format!("pbcopy 실행 실패: {e}"))?;
    child.stdin.take().ok_or("pbcopy 입력을 열지 못했다")?.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_process_names() {
        assert!(looks_like_claude("2.1.310"));
        assert!(looks_like_claude("claude"));
        assert!(!looks_like_claude("zsh"));
        assert!(!looks_like_claude("node"));
        assert!(!looks_like_claude("123"));
    }
}
