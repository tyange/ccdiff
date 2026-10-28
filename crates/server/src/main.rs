//! worktable: Claude Code 세션·워크트리·dev 서버를 한 화면에서 다루는 로컬 웹 앱의 서버.
//! 브라우저는 화면만 그린다. 파일·터미널·git·프로세스는 모두 이 서버가 맡는다.

mod actions;
mod claude;
mod discover;
mod guard;
mod procs;
mod pty;
mod registry;
mod term;
mod tmux;

use std::{env, fs, process::Command, sync::Arc, thread, time::Duration};

use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderValue, StatusCode, Uri},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::watch};

use ccdiff_core::secret::random_token;
use guard::{Guard, WS_PROTOCOL};
use registry::Registry;
use tmux::Tmux;

// web/dist 를 넣은 목록(build.rs 가 만든다). 비어 있으면 임시 페이지를 준다.
include!(concat!(env!("OUT_DIR"), "/assets.rs"));

const PLACEHOLDER: &[(&str, &[u8])] = &[
    ("/index.html", include_bytes!("placeholder/index.html")),
    ("/app.js", include_bytes!("placeholder/app.js")),
];

fn usage() -> &'static str {
    "사용: worktable [--port N] [--open] | worktable doctor\n\
     127.0.0.1 에만 연다. 주소에 붙는 #t= 토큰이 있어야 API·WebSocket 을 쓸 수 있다.\n\
     --open 은 Chrome 앱 창으로 띄운다. WORKTABLE_BROWSER 로 실행 파일을 바꿀 수 있다.\n\
     doctor 는 tmux pane 과 Claude 세션 기록을 어떻게 짝지었는지 출력한다(읽기만 한다)."
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// 화면 파일. 스크립트는 같은 출처에서만 읽고, 다른 사이트가 이 화면을 틀에 넣지 못하게 한다.
async fn static_file(uri: Uri) -> Response {
    let path = match uri.path() {
        "/" => "/index.html",
        p => p,
    };
    let files = if ASSETS.is_empty() { PLACEHOLDER } else { ASSETS };
    let Some((_, body)) = files.iter().find(|(p, _)| *p == path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut resp = ([(header::CONTENT_TYPE, content_type(path))], *body).into_response();
    if path.ends_with(".html") {
        resp.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'"),
        );
    }
    resp
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"ok": true, "version": env!("CARGO_PKG_VERSION")}))
}

/// 갱신 스레드가 내보내는 화면 상태.
#[derive(Clone)]
pub struct AppState {
    sessions: watch::Receiver<Value>,
    tmux: Tmux,
    plans: Arc<actions::Plans>,
    keys: Arc<term::KeySender>,
}

/// 막히는 tmux·git 호출을 돌리고 결과를 JSON 으로. 실패는 409 와 이유.
async fn blocking(f: impl FnOnce() -> Result<Value, String> + Send + 'static) -> Response {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (StatusCode::CONFLICT, Json(json!({"error": e}))).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn field(body: &Value, key: &str) -> String {
    body[key].as_str().unwrap_or_default().to_string()
}

/// 가져오기 계획: {"pane": "%1"} → 경고와 nonce.
async fn import_plan(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let pane = field(&body, "pane");
    blocking(move || actions::plan_import(&st.tmux, &st.plans, &pane)).await
}

/// 확인한 계획 실행: {"nonce": "..."}.
async fn import(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let nonce = field(&body, "nonce");
    blocking(move || actions::import(&st.tmux, &st.plans, &nonce)).await
}

async fn worktrees(State(st): State<AppState>) -> Response {
    let sessions = st.sessions.borrow().clone();
    blocking(move || Ok(Value::Array(actions::worktrees(&sessions)))).await
}

/// 새 세션: {"path": "<워크트리>"}.
async fn new_session(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let (path, sessions) = (field(&body, "path"), st.sessions.borrow().clone());
    blocking(move || actions::new_session(&st.tmux, &sessions, &path)).await
}

async fn sessions(State(st): State<AppState>) -> Json<Value> {
    Json(st.sessions.borrow().clone())
}

/// 화면 갱신을 밀어 주는 통로: 붙자마자 세션 목록을 보내고, 바뀔 때마다 다시 보낸다.
async fn events(State(st): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.protocols([WS_PROTOCOL]).on_upgrade(move |s| serve_events(s, st.sessions))
}

async fn serve_events(mut s: WebSocket, mut sessions: watch::Receiver<Value>) {
    let hello = json!({"type": "hello", "version": env!("CARGO_PKG_VERSION")}).to_string();
    if s.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    loop {
        let msg = json!({"type": "sessions", "sessions": *sessions.borrow_and_update()}).to_string();
        if s.send(Message::Text(msg.into())).await.is_err() {
            return;
        }
        tokio::select! {
            changed = sessions.changed() => if changed.is_err() { return },
            m = s.recv() => match m {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                // 화면에서 오는 메시지는 아직 없다 — 목록을 다시 보내지 않도록 다음 변화까지 기다린다.
                Some(Ok(_)) => if sessions.changed().await.is_err() { return },
            },
        }
    }
}

/// 세션 목록을 2초마다 다시 읽는다. 큰 기록을 처음 읽는 동안에도 API 가 막히지 않게 따로 돈다.
fn spawn_refresh(tmux: Tmux) -> watch::Receiver<Value> {
    let (tx, rx) = watch::channel(Value::Array(Vec::new()));
    thread::spawn(move || {
        let mut reg = Registry::new(tmux);
        loop {
            reg.refresh();
            let snap = reg.snapshot();
            tx.send_if_modified(|cur| {
                let changed = *cur != snap;
                if changed {
                    *cur = snap;
                }
                changed
            });
            if tx.is_closed() {
                return;
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
    rx
}

/// 짝짓기 근거를 사람이 읽게 출력한다.
fn doctor() {
    let tmux = Tmux::from_env();
    let panes = tmux.panes();
    let procs = procs::Procs::snapshot();
    let found = discover::find(&panes, &procs);
    println!("tmux pane {}개, Claude 로 짝지은 pane {}개 (설정 폴더 {})\n", panes.len(), found.len(), claude::config_dir().display());
    for f in &found {
        let p = &f.pane;
        println!("{} {}:{}.{} [{}] pid {} — {}", p.id, p.session, p.window_index, p.window_name, p.command, f.pid, f.how.as_str());
        match &f.file {
            Some(s) => println!(
                "    세션 파일: sessionId {} · status {} · kind {} · tmux 필드 {} · v{}",
                s.session_id,
                s.status,
                s.kind,
                s.tmux.as_deref().unwrap_or("없음"),
                s.version
            ),
            None => println!("    세션 파일 없음 — 명령 {:?} 로 Claude 로 보고 cwd 최근 기록으로 추정", procs.command(f.pid).unwrap_or("")),
        }
        match &f.jsonl {
            Some(j) => {
                let m = fs::metadata(j).ok();
                let size = m.as_ref().map(|m| m.len() / 1024).unwrap_or(0);
                let age = m.and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(0);
                println!("    기록: {} ({size}KB, {age}초 전 수정)", j.display());
            }
            None => println!("    기록: 찾지 못함"),
        }
        if let Some(b) = &f.parked {
            println!("    넘긴 백그라운드 작업: sessionId {} · pid {} · status {} (tmux 밖)", b.session_id, b.pid, b.status);
        }
    }
    let shown: Vec<u32> = found.iter().map(|f| f.pid).chain(found.iter().filter_map(|f| f.parked.as_ref().map(|b| b.pid))).collect();
    let outside: Vec<_> = claude::all_session_files().into_iter().filter(|s| procs.same_process(s.pid, s.proc_start.as_deref()) && !shown.contains(&s.pid)).collect();
    if !outside.is_empty() {
        println!("\ntmux pane 에 없는 실행 중 Claude {}개(목록에 넣지 않음):", outside.len());
        for s in outside {
            println!("    pid {} · {} · kind {} · cwd {}", s.pid, s.session_id, s.kind, s.cwd.display());
        }
    }
}

/// Chrome 을 앱 창으로 띄운다. 이미 떠 있으면 그 Chrome 이 창을 연다.
fn open_app_window(url: &str) {
    let bin = env::var("WORKTABLE_BROWSER")
        .unwrap_or_else(|_| "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
    match Command::new(&bin).arg(format!("--app={url}")).spawn() {
        // 기다리지 않으면 좀비로 남는다 — 따로 거둔다.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("브라우저 실행 실패({bin}): {e} — 위 주소를 직접 여세요"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", usage());
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "doctor") {
        doctor();
        return Ok(());
    }
    let port: u16 = match args.iter().position(|a| a == "--port") {
        Some(i) => args.get(i + 1).context("--port 뒤에 번호가 없다")?.parse().context("--port 는 숫자여야 한다")?,
        None => 0,
    };
    let listener = TcpListener::bind(("127.0.0.1", port)).await.context("127.0.0.1 에 열지 못했다")?;
    let port = listener.local_addr()?.port();
    let token = random_token().context("/dev/urandom 을 읽지 못해 토큰을 만들 수 없다")?;
    let guard = Guard::new(port, token.clone());
    let tmux = Tmux::from_env();
    // 앞서 죽은 서버가 남긴 보기용 세션을 치운다.
    tmux.sweep_views();

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/sessions", get(sessions))
        .route("/api/import/plan", post(import_plan))
        .route("/api/import", post(import))
        .route("/api/worktrees", get(worktrees))
        .route("/api/sessions/new", post(new_session))
        .route("/ws/events", get(events))
        .route("/ws/term/{pane}", get(term::handle))
        .fallback(static_file)
        .with_state(AppState {
            sessions: spawn_refresh(tmux.clone()),
            keys: Arc::new(term::KeySender::new(tmux.clone())),
            tmux,
            plans: Arc::default(),
        })
        .layer(middleware::from_fn_with_state(guard, guard::layer));

    let url = format!("http://127.0.0.1:{port}/#t={token}");
    println!("worktable: {url}");
    if args.iter().any(|a| a == "--open") {
        open_app_window(&url);
    }
    axum::serve(listener, app).await?;
    Ok(())
}
