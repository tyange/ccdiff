//! 시크릿을 화면·로그에 띄우지 않기 위한 판정과, 접속 토큰 만들기.

use std::{fs, io::Read};

/// 자격증명·토큰류 파일 — 내용을 띄우지 않는다. rel 은 워크트리 기준 경로.
pub fn is_secret(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.starts_with(".env")
        || name == ".auth"
        || rel.contains("/.auth")
        || rel.starts_with(".auth")
        || name.contains("credential")
        || name == "token.json"
        || [".pem", ".key", ".p12"].iter().any(|x| name.ends_with(x))
}

/// 16바이트 무작위 값을 16진수로. /dev/urandom 을 못 읽으면 None.
pub fn random_token() -> Option<String> {
    let mut buf = [0u8; 16];
    fs::File::open("/dev/urandom").ok()?.read_exact(&mut buf).ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}
