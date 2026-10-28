//! 시크릿을 화면·로그에 띄우지 않기 위한 판정과, 접속 토큰 만들기.

use std::{fs, io::Read, sync::OnceLock};

use regex::Regex;

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

/// 로그 한 덩어리에서 시크릿처럼 보이는 값을 가린다(`•••`). dev 서버 로그에 키·토큰이 찍힐 수 있어서다.
/// 가리는 것: 이름이 password·secret·token·key 류인 `이름=값`·`이름: 값`, Bearer 토큰, JWT, 잘 알려진 키 접두어,
/// URL 의 `사용자:비밀번호@`, 대소문자·숫자가 섞인 32자 이상 문자열(경로 구분자 `/` 는 끊는다). git 해시처럼 소문자 16진수만인 것은 둔다.
pub fn mask_secrets(text: &str) -> String {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let pats = PATTERNS.get_or_init(|| {
        let r = |p: &str| Regex::new(p).expect("mask pattern");
        vec![
            // 이름=값, 이름: 값, "이름": "값" — 값만 가린다. `Authorization: Bearer <토큰>` 은 토큰까지 한 덩어리로.
            (
                r(r#"(?i)((?:pass(?:word|wd)?|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret|auth(?:orization)?|session|cookie|credential)s?["']?\s*[:=]\s*["']?)((?:bearer\s+|basic\s+)?[^\s"',;&]{4,})"#),
                "${1}•••",
            ),
            (r(r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]{8,}"), "${1}•••"),
            (r(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"), "•••"),
            (
                r(r"\b(?:sk|pk|rk)[-_](?:live|test|proj|ant)?[-_]?[A-Za-z0-9_-]{16,}|\bgh[pousr]_[A-Za-z0-9]{20,}|\bgithub_pat_[A-Za-z0-9_]{20,}|\bxox[abprs]-[A-Za-z0-9-]{10,}|\bAKIA[0-9A-Z]{16}\b|\bAIza[0-9A-Za-z_-]{35}\b|\bglpat-[A-Za-z0-9_-]{20,}"),
                "•••",
            ),
            (r(r"([a-zA-Z][a-zA-Z0-9+.-]*://[^\s:/@]+:)[^\s@/]+@"), "${1}•••@"),
        ]
    });
    static LONG: OnceLock<Regex> = OnceLock::new();
    // `/` 는 넣지 않는다 — 넣으면 `…/Python3.framework/…` 같은 경로 전체가 긴 토큰 하나로 잡힌다.
    let long = LONG.get_or_init(|| Regex::new(r"[A-Za-z0-9+_=-]{32,}").expect("long pattern"));
    let mut out = text.to_string();
    for (re, rep) in pats {
        out = re.replace_all(&out, *rep).into_owned();
    }
    long.replace_all(&out, |c: &regex::Captures| {
        let s = &c[0];
        let mixed = s.chars().any(|ch| ch.is_ascii_uppercase())
            && s.chars().any(|ch| ch.is_ascii_lowercase())
            && s.chars().any(|ch| ch.is_ascii_digit());
        if mixed { "•••".to_string() } else { s.to_string() }
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_named_values() {
        assert_eq!(mask_secrets("DB_PASSWORD=hunter2hunter"), "DB_PASSWORD=•••");
        assert_eq!(mask_secrets(r#"{"apiKey": "abcd1234efgh"}"#), r#"{"apiKey": "•••"}"#);
        assert_eq!(mask_secrets("token: xyz98765"), "token: •••");
        assert_eq!(mask_secrets("Authorization: Bearer abc.def.ghi123"), "Authorization: •••");
        assert_eq!(mask_secrets("curl -H 'x' Bearer abcdefgh12345"), "curl -H 'x' Bearer •••");
    }

    #[test]
    fn masks_known_shapes() {
        assert_eq!(mask_secrets("jwt eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2QT4"), "jwt •••");
        assert_eq!(mask_secrets("key sk-proj-AbCdEfGhIjKlMnOpQrSt"), "key •••");
        assert_eq!(mask_secrets("AKIAABCDEFGHIJKLMNOP"), "•••");
        assert_eq!(mask_secrets("postgres://app:s3cr3t@db:5432/x"), "postgres://app:•••@db:5432/x");
        assert_eq!(mask_secrets("id Xy7kP2mQ9rT4vW8zA1bC3dE5fG6hJ0nL"), "id •••");
    }

    #[test]
    fn keeps_ordinary_log_lines() {
        let lines = [
            "  VITE v8.3.4  ready in 312 ms",
            "  ➜  Local:   http://localhost:5173/",
            "commit 3f2a9c1e5b7d4a6f8e0c2b4d6f8a0c2e4b6d8f0a",
            "GET /api/users/42 200 12ms",
            "src/components/charts/state/loading/CircleSkeleton.tsx",
            "/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/3.9/Python -m http.server",
        ];
        for l in lines {
            assert_eq!(mask_secrets(l), l);
        }
    }
}
