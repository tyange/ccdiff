//! worktable: Claude Code 세션·워크트리·dev 서버를 한 화면에서 다루는 로컬 웹 앱의 서버.
//! 브라우저는 화면만 그린다. 파일·터미널·git·프로세스는 모두 이 서버가 맡는다.

mod guard;

use std::{env, process::Command};

use anyhow::{Context, Result};
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{header, HeaderValue},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use tokio::net::TcpListener;

use ccdiff_core::secret::random_token;
use guard::{Guard, WS_PROTOCOL};

const INDEX: &str = include_str!("placeholder/index.html");
const APP_JS: &str = include_str!("placeholder/app.js");

fn usage() -> &'static str {
    "사용: worktable [--port N] [--open]\n\
     127.0.0.1 에만 연다. 주소에 붙는 #t= 토큰이 있어야 API·WebSocket 을 쓸 수 있다.\n\
     --open 은 Chrome 앱 창으로 띄운다. WORKTABLE_BROWSER 로 실행 파일을 바꿀 수 있다."
}

/// 화면 파일. 스크립트는 같은 출처에서만 읽고, 다른 사이트가 이 화면을 틀에 넣지 못하게 한다.
async fn index() -> Response {
    let mut resp = ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX).into_response();
    resp.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'"),
    );
    resp
}

async fn app_js() -> Response {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], APP_JS).into_response()
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"ok": true, "version": env!("CARGO_PKG_VERSION")}))
}

/// 화면 갱신을 밀어 주는 통로. 지금은 인사만 보낸다.
async fn events(ws: WebSocketUpgrade) -> Response {
    ws.protocols([WS_PROTOCOL]).on_upgrade(serve_events)
}

async fn serve_events(mut s: WebSocket) {
    let hello = json!({"type": "hello", "version": env!("CARGO_PKG_VERSION")}).to_string();
    if s.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    while let Some(Ok(m)) = s.recv().await {
        if matches!(m, Message::Close(_)) {
            break;
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
    let port: u16 = match args.iter().position(|a| a == "--port") {
        Some(i) => args.get(i + 1).context("--port 뒤에 번호가 없다")?.parse().context("--port 는 숫자여야 한다")?,
        None => 0,
    };
    let listener = TcpListener::bind(("127.0.0.1", port)).await.context("127.0.0.1 에 열지 못했다")?;
    let port = listener.local_addr()?.port();
    let token = random_token().context("/dev/urandom 을 읽지 못해 토큰을 만들 수 없다")?;
    let guard = Guard::new(port, token.clone());

    let app = Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/api/health", get(health))
        .route("/ws/events", get(events))
        .layer(middleware::from_fn_with_state(guard, guard::layer));

    let url = format!("http://127.0.0.1:{port}/#t={token}");
    println!("worktable: {url}");
    if args.iter().any(|a| a == "--open") {
        open_app_window(&url);
    }
    axum::serve(listener, app).await?;
    Ok(())
}
