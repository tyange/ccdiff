//! Claude Code `/ide` 가 붙는 작은 MCP 서버 — diff 에서 고른 줄을 selection_changed 로 알린다.
//! Claude Code 는 `~/.claude/ide/<포트>.lock` 을 읽어 IDE 후보를 찾고, workspaceFolders 가 자기 cwd 를
//! 품는 것만 고를 수 있게 보여 준다. 접속은 `ws://127.0.0.1:<포트>` · 하위 프로토콜 `mcp` · 토큰 헤더.

use std::{
    env, fs,
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tungstenite::{
    handshake::server::{ErrorResponse, Request, Response},
    http::{HeaderValue, StatusCode},
    Message,
};

use crate::secret::random_token;

/// 연결마다 (번호, 보낼 통로). 연결이 끊기면 그 스레드가 자기 번호를 지운다.
type Clients = Arc<Mutex<Vec<(u64, Sender<String>)>>>;

pub struct Ide {
    pub port: u16,
    token: String,
    lock: PathBuf,
    folders: Vec<PathBuf>,
    clients: Clients,
    /// 새로 붙은 쪽에 곧바로 보낼 마지막 선택.
    last: Arc<Mutex<Option<String>>>,
}

fn alive(pid: u64) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// 강제 종료된 ccdiff 가 남긴 잠금 파일을 치운다 — 남아 있으면 `/ide` 목록에 죽은 후보가 뜬다.
fn remove_stale(dir: &Path) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(v) = fs::read_to_string(e.path()).map(|t| serde_json::from_str::<Value>(&t).unwrap_or_default()) else { continue };
        if v["ideName"] == "ccdiff" && v["pid"].as_u64().is_some_and(|p| !alive(p)) {
            let _ = fs::remove_file(e.path());
        }
    }
}

impl Ide {
    /// 실패하면 None — 연동만 안 될 뿐 ccdiff 는 그대로 쓴다.
    pub fn start(mut folders: Vec<PathBuf>) -> Option<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").ok()?;
        let port = listener.local_addr().ok()?.port();
        let token = random_token()?;
        let dir = PathBuf::from(env::var_os("HOME")?).join(".claude/ide");
        remove_stale(&dir);
        let lock = dir.join(format!("{port}.lock"));
        let mut seen = std::collections::HashSet::new();
        folders.retain(|f| seen.insert(f.clone()));
        let ide = Self {
            port,
            token: token.clone(),
            lock,
            folders,
            clients: Arc::default(),
            last: Arc::default(),
        };
        ide.write_lock().ok()?;
        let (clients, last) = (Arc::clone(&ide.clients), Arc::clone(&ide.last));
        thread::spawn(move || {
            for (id, stream) in (0u64..).zip(listener.incoming().flatten()) {
                let (token, clients, last) = (token.clone(), Arc::clone(&clients), Arc::clone(&last));
                thread::spawn(move || {
                    serve(id, stream, &token, &clients, &last);
                    if let Ok(mut cs) = clients.lock() {
                        cs.retain(|(i, _)| *i != id);
                    }
                });
            }
        });
        Some(ide)
    }

    fn write_lock(&self) -> std::io::Result<()> {
        if let Some(dir) = self.lock.parent() {
            fs::create_dir_all(dir)?;
        }
        let body = json!({
            "pid": std::process::id(),
            "workspaceFolders": self.folders,
            "ideName": "ccdiff",
            "transport": "ws",
            "authToken": self.token,
        });
        fs::write(&self.lock, body.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.lock, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// 세션이 새 워크트리를 다루기 시작하면 그 워크트리의 Claude 도 고를 수 있게 한다.
    pub fn add_folder(&mut self, p: &Path) {
        if !self.folders.iter().any(|f| f == p) {
            self.folders.push(p.to_path_buf());
            let _ = self.write_lock();
        }
    }

    pub fn connected(&self) -> usize {
        self.clients.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// lines 는 0부터 센 [start, end] 줄. None 이면 선택을 비운다.
    pub fn select(&self, path: &Path, lines: Option<(usize, usize)>, text: &str) {
        let params = match lines {
            Some((s, e)) => json!({
                "text": text,
                "filePath": path,
                "fileUrl": format!("file://{}", path.display()),
                "selection": {
                    "start": {"line": s, "character": 0},
                    "end": {"line": e, "character": text.lines().last().map(|l| l.chars().count()).unwrap_or(0).max(1)},
                    "isEmpty": false,
                },
            }),
            // 경로까지 비운다 — 경로만 남기면 Claude 가 "그 파일을 열어 둔 상태" 로 받아들인다.
            None => json!({
                "selection": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}, "isEmpty": true},
            }),
        };
        let msg = json!({"jsonrpc": "2.0", "method": "selection_changed", "params": params}).to_string();
        if let Ok(mut l) = self.last.lock() {
            *l = lines.map(|_| msg.clone());
        }
        if let Ok(mut cs) = self.clients.lock() {
            cs.retain(|(_, tx)| tx.send(msg.clone()).is_ok());
        }
    }
}

impl Drop for Ide {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock);
    }
}

fn serve(id: u64, stream: TcpStream, token: &str, clients: &Clients, last: &Mutex<Option<String>>) {
    let check = |req: &Request, mut resp: Response| {
        let ok = req
            .headers()
            .get("x-claude-code-ide-authorization")
            .is_some_and(|v| v.as_bytes() == token.as_bytes());
        if !ok {
            let mut err = ErrorResponse::new(None);
            *err.status_mut() = StatusCode::UNAUTHORIZED;
            return Err(err);
        }
        resp.headers_mut().insert("Sec-WebSocket-Protocol", HeaderValue::from_static("mcp"));
        Ok(resp)
    };
    let Ok(mut ws) = tungstenite::accept_hdr(stream, check) else { return };
    // 읽기를 짧게 끊어, 같은 스레드에서 보낼 알림도 처리한다.
    let _ = ws.get_mut().set_read_timeout(Some(Duration::from_millis(100)));
    let (tx, rx): (Sender<String>, Receiver<String>) = mpsc::channel();
    let mut registered = Some(tx);
    // Claude 는 초기화 직후에야 선택 알림 처리기를 단다 — 그 전에 보낸 것은 버려지므로 조금 뒤 다시 보낸다.
    let mut resend_at = None;
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let Ok(req) = serde_json::from_str::<Value>(&t) else { continue };
                if let Some(reply) = answer(&req) {
                    if ws.send(Message::text(reply.to_string())).is_err() {
                        break;
                    }
                }
                // 초기화가 끝난 뒤부터 알림을 받는다.
                if req["method"] == "notifications/initialized" {
                    if let Some(tx) = registered.take() {
                        if let Ok(mut cs) = clients.lock() {
                            cs.push((id, tx));
                        }
                        resend_at = Some(Instant::now() + Duration::from_millis(1500));
                    }
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
        if resend_at.is_some_and(|t| Instant::now() >= t) {
            resend_at = None;
            if let Some(m) = last.lock().ok().and_then(|l| l.clone()) {
                if ws.send(Message::text(m)).is_err() {
                    return;
                }
            }
        }
        while let Ok(m) = rx.try_recv() {
            if ws.send(Message::text(m)).is_err() {
                return;
            }
        }
    }
}

/// 도구는 내놓지 않는다 — 선택 알림만 쓰는 연결이다.
fn answer(req: &Value) -> Option<Value> {
    let id = req.get("id")?.clone();
    let result = match req["method"].as_str()? {
        "initialize" => json!({
            "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "ccdiff", "version": env!("CARGO_PKG_VERSION")},
        }),
        "tools/list" => json!({"tools": []}),
        "prompts/list" => json!({"prompts": []}),
        "resources/list" => json!({"resources": []}),
        "ping" => json!({}),
        _ => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}
