//! 웹 터미널 칸. 고른 pane 의 window 를 보기용 세션(원래 세션과 묶인 grouped session)에서 `tmux attach` 로 보여 준다.
//! tmux 가 바깥 터미널(xterm.js)을 상대로 마우스·붙여넣기·확장 키·대체 화면을 알아서 처리하므로, pane 출력만
//! 받아 그리는 control mode 보다 상태를 맞출 일이 없다. 칸마다 보기용 세션이 따로 있어 보는 window 가 서로 독립이다.
//! - 앱 세션(worktable)의 window: pane 이 하나뿐이다. 입력을 PTY 에 그대로 쓰고, PTY 크기가 window 크기가 된다.
//! - 다른 세션(abc 등)의 window: `-f ignore-size` 로 붙어 내 터미널의 크기를 건드리지 않는다. window 에 pane 이
//!   여럿일 수 있어 입력은 `send-keys -H` 로 그 pane 에만 보내고 마우스 보고는 버린다. 활성 pane 은 window 단위로
//!   공유되므로 select-pane 은 하지 않는다.

use std::{
    io::{self, Read, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
    thread,
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, RawQuery, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::{
    guard::WS_PROTOCOL,
    pty::Pty,
    tmux::{Tmux, HOME_SESSION, VIEW_PREFIX},
    AppState,
};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// 보기용 세션. 칸이 닫히면 지운다(서버가 죽어도 destroy-unattached 로 사라진다).
struct View {
    tmux: Tmux,
    name: String,
}

impl Drop for View {
    fn drop(&mut self) {
        self.tmux.run(&["kill-session", "-t", &format!("={}", self.name)]);
    }
}

struct Term {
    // 필드는 선언 순서로 버려진다 — tmux 클라이언트를 먼저 끄고 보기용 세션을 지운다.
    pty: Pty,
    _view: View,
    owned: bool,
    pane: String,
    session: String,
}

fn size(q: Option<&str>) -> (u16, u16) {
    let mut cols = 80;
    let mut rows = 24;
    for kv in q.unwrap_or("").split('&') {
        match kv.split_once('=') {
            Some(("cols", v)) => cols = v.parse().unwrap_or(cols),
            Some(("rows", v)) => rows = v.parse().unwrap_or(rows),
            _ => {}
        }
    }
    (cols.clamp(10, 500), rows.clamp(4, 200))
}

fn open(tmux: &Tmux, pane_id: &str, cols: u16, rows: u16) -> io::Result<Term> {
    let not_found = || io::Error::new(io::ErrorKind::NotFound, "pane 이 없다");
    let pane = tmux.pane(pane_id).ok_or_else(not_found)?;
    let owned = pane.session == HOME_SESSION;
    let name = format!("{VIEW_PREFIX}{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    tmux.run(&["new-session", "-d", "-t", &format!("={}", pane.session), "-s", &name])
        .ok_or_else(|| io::Error::other("보기용 세션을 만들지 못했다"))?;
    let view = View { tmux: tmux.clone(), name: name.clone() };
    tmux.run(&["select-window", "-t", &format!("={name}:{}", pane.window_id)]).ok_or_else(not_found)?;
    let mut cmd = tmux.command();
    cmd.args(["attach-session", "-t", &format!("={name}")]);
    if !owned {
        cmd.args(["-f", "ignore-size"]);
    }
    // 옵션은 붙은 뒤에 켠다: destroy-unattached 는 클라이언트 없는 세션에 켜는 순간 세션을 지운다(tmux 3.8 실측).
    // set-option -t 는 `=이름` 표기를 받지 않아 이름만 쓴다.
    cmd.args([";", "set-option", "-t", &name, "status", "off", ";", "set-option", "-t", &name, "destroy-unattached", "on"]);
    // 서버를 tmux 안에서 띄웠어도 중첩 attach 로 거부되지 않게 TMUX 를 뺀다.
    cmd.env_remove("TMUX").env("TERM", "xterm-256color").env("COLORTERM", "truecolor");
    let pty = Pty::spawn(cmd, cols, rows)?;
    Ok(Term { pty, _view: view, owned, pane: pane.id, session: pane.session })
}

/// xterm.js 가 보내는 마우스 보고(SGR `ESC[<…M/m`, X10 `ESC[M` + 3바이트)와 포커스 보고(`ESC[I`·`ESC[O`)를 뺀다.
/// 다른 세션 window 에 send-keys 로 보낼 때만 쓴다 — 바깥 tmux 가 켠 모드라 Claude 에 그대로 가면 글자로 찍힌다.
fn strip_reports(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"\x1b[<") {
            match b[i..].iter().position(|&c| c == b'M' || c == b'm') {
                Some(end) => i += end + 1,
                None => break,
            }
        } else if b[i..].starts_with(b"\x1b[M") {
            i += 6;
        } else if b[i..].starts_with(b"\x1b[I") || b[i..].starts_with(b"\x1b[O") {
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn send_keys(tmux: &Tmux, pane: &str, bytes: &[u8]) {
    let bytes = strip_reports(bytes);
    if bytes.is_empty() {
        return;
    }
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut args = vec!["send-keys", "-t", pane, "-H"];
    args.extend(hex.iter().map(String::as_str));
    tmux.run(&args);
}

pub async fn handle(
    State(st): State<AppState>,
    Path(pane): Path<String>,
    RawQuery(q): RawQuery,
    ws: WebSocketUpgrade,
) -> Response {
    // 목록에 있는 Claude pane 에만 붙는다.
    let known = st.sessions.borrow().as_array().is_some_and(|a| a.iter().any(|s| s["pane"]["id"] == pane.as_str()));
    if !known {
        return (StatusCode::NOT_FOUND, "목록에 없는 pane").into_response();
    }
    let (cols, rows) = size(q.as_deref());
    ws.protocols([WS_PROTOCOL]).on_upgrade(move |s| serve(s, st.tmux.clone(), pane, cols, rows))
}

async fn serve(mut ws: WebSocket, tmux: Tmux, pane: String, cols: u16, rows: u16) {
    let t = tmux.clone();
    let term = match tokio::task::spawn_blocking(move || open(&t, &pane, cols, rows)).await {
        Ok(Ok(term)) => term,
        Ok(Err(e)) => {
            let _ = ws.send(Message::Text(json!({"type": "error", "message": e.to_string()}).to_string().into())).await;
            return;
        }
        Err(_) => return,
    };
    let (Ok(mut reader), Ok(mut writer)) = (term.pty.master.try_clone(), term.pty.master.try_clone()) else { return };

    // PTY 읽기·쓰기는 막히는 호출이라 스레드에서 한다.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let (in_tx, in_rx) = std_mpsc::channel::<Vec<u8>>();
    let (owned, pane) = (term.owned, term.pane.clone());
    let keys_tmux = tmux.clone();
    thread::spawn(move || {
        for b in in_rx {
            let ok = if owned {
                writer.write_all(&b).is_ok()
            } else {
                send_keys(&keys_tmux, &pane, &b);
                true
            };
            if !ok {
                return;
            }
        }
    });

    let hello = json!({"type": "attached", "pane": term.pane, "session": term.session, "owned": term.owned});
    if ws.send(Message::Text(hello.to_string().into())).await.is_ok() {
        loop {
            tokio::select! {
                out = out_rx.recv() => match out {
                    Some(b) => if ws.send(Message::Binary(b.into())).await.is_err() { break },
                    // tmux 클라이언트가 끝났다(pane·세션이 사라짐).
                    None => break,
                },
                m = ws.recv() => match m {
                    Some(Ok(Message::Binary(b))) => if in_tx.send(b.to_vec()).is_err() { break },
                    Some(Ok(Message::Text(t))) => control(&term, &t),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                },
            }
        }
    }
    drop(in_tx);
    // tmux 명령을 부르므로 막혀도 되는 곳에서 정리한다.
    let _ = tokio::task::spawn_blocking(move || drop(term)).await;
}

/// 화면에서 오는 제어 메시지: {"type":"resize","cols":N,"rows":N}
fn control(term: &Term, text: &str) {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return };
    if v["type"] == "resize" {
        let n = |k: &str| v[k].as_u64().map(|x| x.clamp(4, 500) as u16);
        if let (Some(c), Some(r)) = (n("cols"), n("rows")) {
            let _ = term.pty.resize(c, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_mouse_and_focus_reports() {
        assert_eq!(strip_reports(b"ab\x1b[<0;10;5Mcd\x1b[<0;10;5m"), b"abcd");
        assert_eq!(strip_reports(b"\x1b[M !!x"), b"x");
        assert_eq!(strip_reports(b"\x1b[Ihi\x1b[O"), b"hi");
        // 방향키 등 다른 이스케이프는 그대로.
        assert_eq!(strip_reports(b"\x1b[A\r"), b"\x1b[A\r");
    }

    #[test]
    fn size_from_query() {
        assert_eq!(size(Some("cols=120&rows=40")), (120, 40));
        assert_eq!(size(None), (80, 24));
        assert_eq!(size(Some("cols=9999&rows=1")), (500, 4));
    }
}
