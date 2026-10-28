//! 접속 검사. 이 서버는 터미널에 키를 보낼 수 있다 — 열려 있으면 원격 실행 창구가 된다.
//! - Host: 127.0.0.1·localhost 의 이 포트만. 다른 이름으로 들어온 요청(DNS rebinding)을 막는다.
//! - Origin: 있으면 이 서버 주소여야 한다. WebSocket 은 반드시 있어야 한다(다른 사이트가 여는 것을 막는다).
//! - 토큰: `/api/` 는 `Authorization: Bearer`, `/ws/` 는 `Sec-WebSocket-Protocol` 의 `auth.<토큰>`.
//!   브라우저 WebSocket 은 헤더를 못 붙여 하위 프로토콜로 싣는다. 쿠키는 포트를 가리지 않아 쓰지 않는다.
//!
//! 화면 파일(`/`, 스크립트)은 토큰 없이 준다 — 내용이 공개 코드이고, 토큰은 주소의 `#t=` 로 페이지에 건넨다.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// 화면이 고르는 하위 프로토콜. 서버는 이것만 돌려준다(토큰을 되돌려 보내지 않는다).
pub const WS_PROTOCOL: &str = "worktable.v1";

#[derive(Clone)]
pub struct Guard {
    port: u16,
    token: Arc<str>,
}

/// 길이가 같으면 모든 바이트를 비교한다 — 앞에서 몇 글자 맞는지 시간으로 새지 않게.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Guard {
    pub fn new(port: u16, token: String) -> Self {
        Self { port, token: token.into() }
    }

    fn host_ok(&self, host: &str) -> bool {
        host == format!("127.0.0.1:{}", self.port) || host == format!("localhost:{}", self.port)
    }

    fn origin_ok(&self, origin: &str) -> bool {
        origin == format!("http://127.0.0.1:{}", self.port) || origin == format!("http://localhost:{}", self.port)
    }

    fn token_ok(&self, given: &str) -> bool {
        same(given.as_bytes(), self.token.as_bytes())
    }

    /// 요청 하나를 들일지 정한다.
    pub fn decide(&self, path: &str, headers: &HeaderMap) -> Result<(), StatusCode> {
        let get = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
        if !get(header::HOST).is_some_and(|h| self.host_ok(h)) {
            return Err(StatusCode::FORBIDDEN);
        }
        let origin = get(header::ORIGIN);
        if origin.is_some_and(|o| !self.origin_ok(o)) {
            return Err(StatusCode::FORBIDDEN);
        }
        if path.starts_with("/api/") {
            let bearer = get(header::AUTHORIZATION).and_then(|v| v.strip_prefix("Bearer "));
            if !bearer.is_some_and(|t| self.token_ok(t)) {
                return Err(StatusCode::UNAUTHORIZED);
            }
        } else if path.starts_with("/ws/") {
            if origin.is_none() {
                return Err(StatusCode::FORBIDDEN);
            }
            let offered = get(header::SEC_WEBSOCKET_PROTOCOL).unwrap_or("");
            let mut protos = offered.split(',').map(str::trim);
            if !protos.any(|p| p.strip_prefix("auth.").is_some_and(|t| self.token_ok(t))) {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
        Ok(())
    }
}

/// 모든 경로에 거는 미들웨어. 응답에는 캐시·형식 추측을 막는 헤더를 붙인다.
pub async fn layer(State(g): State<Guard>, req: Request, next: Next) -> Response {
    if let Err(code) = g.decide(req.uri().path(), req.headers()) {
        return code.into_response();
    }
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn g() -> Guard {
        Guard::new(4321, TOKEN.into())
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(header::HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn host_must_be_this_server() {
        assert!(g().decide("/", &headers(&[("host", "127.0.0.1:4321")])).is_ok());
        assert!(g().decide("/", &headers(&[("host", "localhost:4321")])).is_ok());
        // DNS rebinding: 다른 이름이 127.0.0.1 로 풀려도 Host 가 다르다.
        assert_eq!(g().decide("/", &headers(&[("host", "evil.example:4321")])), Err(StatusCode::FORBIDDEN));
        assert_eq!(g().decide("/", &headers(&[("host", "127.0.0.1:9999")])), Err(StatusCode::FORBIDDEN));
        assert_eq!(g().decide("/", &headers(&[])), Err(StatusCode::FORBIDDEN));
    }

    #[test]
    fn foreign_origin_is_refused() {
        let h = headers(&[("host", "127.0.0.1:4321"), ("origin", "https://evil.example"), ("authorization", &format!("Bearer {TOKEN}"))]);
        assert_eq!(g().decide("/api/health", &h), Err(StatusCode::FORBIDDEN));
        let h = headers(&[("host", "127.0.0.1:4321"), ("origin", "http://127.0.0.1:4321"), ("authorization", &format!("Bearer {TOKEN}"))]);
        assert!(g().decide("/api/health", &h).is_ok());
    }

    #[test]
    fn api_needs_token() {
        let base = [("host", "127.0.0.1:4321")];
        assert_eq!(g().decide("/api/health", &headers(&base)), Err(StatusCode::UNAUTHORIZED));
        let wrong = headers(&[base[0], ("authorization", "Bearer 0123456789abcdef0123456789abcdee")]);
        assert_eq!(g().decide("/api/health", &wrong), Err(StatusCode::UNAUTHORIZED));
        let short = headers(&[base[0], ("authorization", "Bearer 0123")]);
        assert_eq!(g().decide("/api/health", &short), Err(StatusCode::UNAUTHORIZED));
        let ok = headers(&[base[0], ("authorization", &format!("Bearer {TOKEN}"))]);
        assert!(g().decide("/api/health", &ok).is_ok());
    }

    #[test]
    fn websocket_needs_origin_and_token() {
        let proto = format!("{WS_PROTOCOL}, auth.{TOKEN}");
        let no_origin = headers(&[("host", "127.0.0.1:4321"), ("sec-websocket-protocol", &proto)]);
        assert_eq!(g().decide("/ws/events", &no_origin), Err(StatusCode::FORBIDDEN));
        let no_token = headers(&[("host", "127.0.0.1:4321"), ("origin", "http://127.0.0.1:4321"), ("sec-websocket-protocol", WS_PROTOCOL)]);
        assert_eq!(g().decide("/ws/events", &no_token), Err(StatusCode::UNAUTHORIZED));
        let ok = headers(&[("host", "127.0.0.1:4321"), ("origin", "http://127.0.0.1:4321"), ("sec-websocket-protocol", &proto)]);
        assert!(g().decide("/ws/events", &ok).is_ok());
    }
}
