//! web/dist(화면 빌드 결과)를 바이너리에 넣는다. 파일 목록을 include_bytes! 로 만든다.
//! web/dist 가 없으면 빈 목록 — 서버는 임시 페이지를 준다.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, base, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            out.push((rel.to_string_lossy().replace('\\', "/"), p.canonicalize().unwrap_or(p.clone())));
        }
    }
}

fn main() {
    let dist = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir")).join("../../web/dist");
    // 폴더를 주면 cargo 가 안의 파일 변경까지 본다.
    println!("cargo:rerun-if-changed={}", dist.display());
    let mut files = Vec::new();
    if dist.is_dir() {
        walk(&dist, &dist, &mut files);
    }
    files.sort();
    let mut src = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for (rel, abs) in files {
        src += &format!("    ({:?}, include_bytes!({:?})),\n", format!("/{rel}"), abs);
    }
    src += "];\n";
    fs::write(PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("assets.rs"), src).expect("assets.rs");
}
