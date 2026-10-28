//! worktable: Claude Code 세션·워크트리·dev 서버를 한 화면에서 다루는 로컬 웹 앱의 서버.
//! 브라우저는 화면만 그린다. 파일·터미널·git·프로세스는 모두 이 서버가 맡는다.

mod actions;
mod changes;
mod claude;
mod discover;
mod guard;
mod procs;
mod pty;
mod registry;
mod servers;
mod term;
mod tmux;

use std::{
    env, fs,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

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

use ccdiff_core::{ide::Ide, secret::random_token};
use guard::{Guard, WS_PROTOCOL};
use registry::{Refresher, Selection, Shared};
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
        "woff" => "font/woff",
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
        // Monaco 가 실행 중에 <style> 을 넣어 style-src 에 'unsafe-inline' 이 필요하다. 이 화면은 바깥 HTML 을 그리지 않는다.
        resp.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; \
                 worker-src 'self' blob:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'",
            ),
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
    reg: Shared,
    ide: Option<Arc<Mutex<Ide>>>,
    tmux: Tmux,
    plans: Arc<actions::Plans>,
    keys: Arc<term::KeySender>,
    remembered: Arc<servers::Remembered>,
    restarts: Arc<servers::RestartPlans>,
}

/// 레지스트리에서 서버 짝짓기에 쓸 것만 잠깐 잠가 복사한다.
fn known(st: &AppState) -> Result<servers::Known, String> {
    Ok(servers::Known::from(&*st.reg.lock().map_err(|_| "잠금 실패")?))
}

async fn servers_list(State(st): State<AppState>) -> Response {
    blocking(move || Ok(Value::Array(servers::list(&st.tmux, &known(&st)?, &st.remembered)))).await
}

/// 재시작 계획: {"pane"} → 다시 칠 명령·경고·nonce.
async fn restart_plan(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let pane = field(&body, "pane");
    blocking(move || servers::plan_restart(&st.tmux, &known(&st)?, &st.remembered, &st.restarts, &pane)).await
}

/// 확인한 재시작 실행: {"nonce"}.
async fn restart(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let nonce = field(&body, "nonce");
    blocking(move || servers::restart(&st.tmux, &st.restarts, &nonce)).await
}

/// 서버 pane 로그: 1초마다 읽어 바뀌었을 때만 {"type":"log","text"} 를 보낸다. 시크릿은 서버에서 가린다.
async fn log_ws(State(st): State<AppState>, axum::extract::Path(pane): axum::extract::Path<String>, ws: WebSocketUpgrade) -> Response {
    let tmux = st.tmux.clone();
    let p = pane.clone();
    let ok = tokio::task::spawn_blocking(move || tmux.pane(&p).is_some_and(|p| servers::is_server_pane(&p))).await.unwrap_or(false);
    if !ok {
        return (StatusCode::NOT_FOUND, "서버 pane 이 아니다").into_response();
    }
    ws.protocols([WS_PROTOCOL]).on_upgrade(move |s| serve_log(s, st.tmux, pane))
}

async fn serve_log(mut s: WebSocket, tmux: Tmux, pane: String) {
    let mut last = String::new();
    loop {
        let (t, p) = (tmux.clone(), pane.clone());
        let Ok(Some(text)) = tokio::task::spawn_blocking(move || servers::log(&t, &p)).await else {
            let _ = s.send(Message::Text(json!({"type": "gone"}).to_string().into())).await;
            return;
        };
        if text != last {
            if s.send(Message::Text(json!({"type": "log", "text": text}).to_string().into())).await.is_err() {
                return;
            }
            last = text;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            m = s.recv() => if matches!(m, Some(Ok(Message::Close(_))) | None | Some(Err(_))) { return },
        }
    }
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
    let ide_port = st.ide.as_ref().and_then(|i| i.lock().ok().map(|i| i.port));
    blocking(move || actions::new_session(&st.tmux, &sessions, &path, ide_port)).await
}

/// 세션의 변경 목록: {"key", "all"}. all 이면 세션 시작 전부터 있던 작업 트리 변경도.
async fn changes_list(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let (key, all) = (field(&body, "key"), body["all"].as_bool().unwrap_or(false));
    blocking(move || changes::list(&*st.reg.lock().map_err(|_| "잠금 실패")?, &key, all)).await
}

/// 항목 하나의 수정 전후: {"key", "id"}.
async fn changes_diff(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let (key, id) = (field(&body, "key"), field(&body, "id"));
    blocking(move || changes::diff(&*st.reg.lock().map_err(|_| "잠금 실패")?, &key, &id)).await
}

/// 편집기로 열기: {"key", "id", "line"}.
async fn open_editor(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let (key, id, line) = (field(&body, "key"), field(&body, "id"), body["line"].as_u64().unwrap_or(1) as usize);
    blocking(move || changes::open_in_editor(&*st.reg.lock().map_err(|_| "잠금 실패")?, &key, &id, line)).await
}

/// 고른 줄을 그 세션의 Claude 에 알린다: {"key", "path", "start", "end", "text"} (줄은 1부터), 비우기는 {"key", "clear": true}.
/// 그 세션 Claude 에 붙은 IDE 연결에만 보낸다. 나중에 붙는 연결에는 갱신 스레드가 마지막 선택을 다시 보낸다.
async fn selection(State(st): State<AppState>, Json(body): Json<Value>) -> Response {
    let key = field(&body, "key");
    let path = PathBuf::from(field(&body, "path"));
    let text = field(&body, "text");
    let lines = match (body["start"].as_u64(), body["end"].as_u64()) {
        (Some(a), Some(b)) if !body["clear"].as_bool().unwrap_or(false) && a >= 1 && b >= a => Some((a as usize - 1, b as usize - 1)),
        _ => None,
    };
    let ids = {
        let Ok(mut reg) = st.reg.lock() else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
        match lines {
            Some(l) => reg.last_sel.insert(key.clone(), Selection { path: path.clone(), lines: l, text: text.clone() }),
            None => reg.last_sel.remove(&key),
        };
        reg.ide_clients(&key)
    };
    if let Some(ide) = st.ide.as_ref().and_then(|i| i.lock().ok()) {
        ide.select_to(&ids, &path, lines, &text);
    }
    Json(json!({"sent": ids.len()})).into_response()
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

/// 세션 목록을 2초마다 다시 읽는다. 오래 걸리는 일은 잠금 밖에서 해 API 가 막히지 않는다.
fn spawn_refresh(tmux: Tmux, reg: Shared, ide: Option<Arc<Mutex<Ide>>>) -> watch::Receiver<Value> {
    let (tx, rx) = watch::channel(Value::Array(Vec::new()));
    thread::spawn(move || {
        let mut r = Refresher::new(tmux, ide);
        loop {
            r.refresh(&reg);
            let Some(snap) = reg.lock().ok().map(|g| g.snapshot()) else { return };
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

/// 지난번 IDE 포트를 기억하는 파일 — 다시 시작해도 같은 포트를 쓰려고.
fn ide_port_file() -> Option<PathBuf> {
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("worktable/ide-port"))
}

/// IDE 를 연다. CLAUDE_CODE_SSE_PORT 로 띄운 Claude 는 그 포트에만 다시 붙으므로 지난번 포트를 먼저 시도한다.
fn start_ide() -> Option<Arc<Mutex<Ide>>> {
    let file = ide_port_file();
    let prev = file.as_ref().and_then(|f| fs::read_to_string(f).ok()).and_then(|s| s.trim().parse().ok());
    let ide = Ide::start_on("worktable", Vec::new(), prev)?;
    if let Some(f) = &file {
        let _ = f.parent().map(fs::create_dir_all);
        let _ = fs::write(f, ide.port.to_string());
    }
    Some(Arc::new(Mutex::new(ide)))
}

/// Ctrl-C·SIGTERM 을 기다린다 — 끝날 때 IDE 잠금 파일을 지우려고 정상 종료한다.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM 처리기");
    tokio::select! {
        _ = ctrl_c => {},
        _ = term.recv() => {},
    }
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

    // Claude Code 의 /ide 가 붙는 곳. 세션마다 그 Claude 의 연결에만 선택을 보낸다.
    let ide = start_ide();
    if ide.is_none() {
        eprintln!("IDE 연결을 열지 못했다 — 줄 선택을 Claude 에 보낼 수 없다");
    }
    let reg: Shared = Arc::default();

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/sessions", get(sessions))
        .route("/api/import/plan", post(import_plan))
        .route("/api/import", post(import))
        .route("/api/worktrees", get(worktrees))
        .route("/api/sessions/new", post(new_session))
        .route("/api/changes", post(changes_list))
        .route("/api/diff", post(changes_diff))
        .route("/api/open", post(open_editor))
        .route("/api/selection", post(selection))
        .route("/api/servers", get(servers_list))
        .route("/api/servers/restart/plan", post(restart_plan))
        .route("/api/servers/restart", post(restart))
        .route("/ws/log/{pane}", get(log_ws))
        .route("/ws/events", get(events))
        .route("/ws/term/{pane}", get(term::handle))
        .fallback(static_file)
        .with_state(AppState {
            sessions: spawn_refresh(tmux.clone(), reg.clone(), ide.clone()),
            reg,
            ide: ide.clone(),
            keys: Arc::new(term::KeySender::new(tmux.clone())),
            tmux,
            plans: Arc::default(),
            remembered: Arc::default(),
            restarts: Arc::default(),
        })
        .layer(middleware::from_fn_with_state(guard, guard::layer));

    let url = format!("http://127.0.0.1:{port}/#t={token}");
    println!("worktable: {url}");
    if args.iter().any(|a| a == "--open") {
        open_app_window(&url);
    }
    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;
    // 갱신 스레드도 IDE 를 들고 있어 Drop 이 돌지 않는다 — 잠금 파일은 직접 지운다.
    if let Some(i) = ide.as_ref().and_then(|i| i.lock().ok()) {
        i.remove_lock();
    }
    Ok(())
}
