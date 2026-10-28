//! 변경 칸: 세션의 수정 목록(✓◐●✗)과 작업 트리 항목(◆), 항목 하나의 수정 전후 파일, 편집기로 열기.
//! 항목 ID — 세션 수정은 `e:<번호>`, 작업 트리 항목은 `w:<워크트리>|<상대 경로>`(10초마다 다시 읽어도 같게).
//! 시크릿 파일(.env·키 등)은 이름만 보이고 내용은 주지 않는다 — 세션 수정이라도 마찬가지다.

use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{json, Value};

use ccdiff_core::{
    git::CommitState,
    rows::{build_rows, RowKind},
    secret::is_secret,
    session::EditEntry,
};

use crate::registry::{Entry, Registry};

fn mark(st: Option<&CommitState>) -> (&'static str, Value) {
    match st {
        Some(CommitState::Yes { hash, subject }) => ("committed", json!({"hash": hash, "subject": subject})),
        Some(CommitState::Changed { hash, subject }) => ("changed", json!({"hash": hash, "subject": subject})),
        Some(CommitState::No) => ("uncommitted", Value::Null),
        Some(CommitState::Gone) => ("gone", Value::Null),
        _ => ("unknown", Value::Null),
    }
}

fn item_json(id: String, e: &EditEntry, mark: &str, commit: Value) -> Value {
    json!({
        "id": id,
        "kind": e.kind,
        "path": e.path,
        "rel": e.rel_path(),
        "root": e.root,
        "worktree": e.root.as_ref().and_then(|r| r.file_name()).map(|n| n.to_string_lossy().into_owned()),
        "epoch": e.epoch,
        "added": e.added,
        "removed": e.removed,
        "mark": mark,
        "commit": commit,
        "userModified": e.user_modified,
        "secret": is_secret(&e.rel_path()),
    })
}

fn wt_id(e: &EditEntry) -> String {
    format!("w:{}|{}", e.root.as_ref().map(|r| r.display().to_string()).unwrap_or_default(), e.rel_path())
}

/// 세션이 다룬 워크트리의 작업 트리 항목. all 이 아니면 세션 시작 전부터 있던 변경은 뺀다(ccdiff 와 같다).
fn wt_items<'a>(reg: &'a Registry, e: &Entry, all: bool) -> Vec<&'a EditEntry> {
    let since = if all { i64::MIN } else { e.tracker.as_ref().map(|t| t.session_start).unwrap_or(0) - 60 };
    e.roots()
        .iter()
        .filter_map(|r| reg.wt.get(r))
        .flat_map(|w| &w.items)
        // 지워진 파일은 mtime 이 없어 0 이다 — 시각으로 거르지 않는다.
        .filter(|x| x.epoch == 0 || x.epoch >= since)
        .collect()
}

/// 시간순 목록. git 밖 파일은 뺀다(ccdiff 기본과 같다).
pub fn list(reg: &Registry, key: &str, all: bool) -> Result<Value, String> {
    let e = reg.entries.get(key).ok_or("세션이 없다")?;
    let mut items: Vec<(i64, Value)> = Vec::new();
    if let Some(t) = &e.tracker {
        for (i, x) in t.entries.iter().enumerate().filter(|(_, x)| x.root.is_some()) {
            let (m, c) = mark(e.commits.get(&i));
            items.push((x.epoch, item_json(format!("e:{i}"), x, m, c)));
        }
    }
    for x in wt_items(reg, e, all) {
        items.push((x.epoch, item_json(wt_id(x), x, "worktree", Value::Null)));
    }
    items.sort_by_key(|(t, _)| *t);
    Ok(Value::Array(items.into_iter().map(|(_, v)| v).collect()))
}

fn find<'a>(reg: &'a Registry, e: &'a Entry, id: &str) -> Option<&'a EditEntry> {
    if let Some(i) = id.strip_prefix("e:") {
        return e.tracker.as_ref()?.entries.get(i.parse::<usize>().ok()?);
    }
    wt_items(reg, e, true).into_iter().find(|x| wt_id(x) == id)
}

/// 항목 하나의 수정 전후 파일. 같은 파일을 고친 다른 항목 ID 들(seq)도 같이 준다 — 앞뒤로 넘기기용.
pub fn diff(reg: &Registry, key: &str, id: &str) -> Result<Value, String> {
    let e = reg.entries.get(key).ok_or("세션이 없다")?;
    let x = find(reg, e, id).ok_or("항목이 없다 — 목록이 바뀌었다")?;
    let mut seq: Vec<String> = e
        .tracker
        .as_ref()
        .map(|t| t.entries.iter().enumerate().filter(|(_, y)| y.path == x.path).map(|(i, _)| format!("e:{i}")).collect())
        .unwrap_or_default();
    seq.extend(wt_items(reg, e, true).into_iter().filter(|y| y.path == x.path).map(wt_id));
    let base = json!({"id": id, "path": x.path, "rel": x.rel_path(), "root": x.root, "kind": x.kind, "seq": seq});
    if is_secret(&x.rel_path()) {
        return Ok(json!({"secret": true, "info": base}));
    }
    let (rows, _) = build_rows(x);
    let pick = |skip: RowKind| rows.iter().filter(|r| r.kind != skip && r.kind != RowKind::Sep).map(|r| r.text.as_str()).collect::<Vec<_>>().join("\n");
    // 수정 전 파일이 기록에 없으면 diff 조각만 있다 — 화면이 "일부" 로 표시한다.
    let partial = x.before.is_none() && x.kind != "Write+";
    let first = rows.iter().filter(|r| r.kind != RowKind::Del).position(|r| r.kind == RowKind::Add).map(|i| i + 1);
    Ok(json!({
        "secret": false,
        "info": base,
        "original": pick(RowKind::Add),
        "modified": pick(RowKind::Del),
        "partial": partial,
        "firstChange": first,
    }))
}

/// 데스크톱 편집기로 그 줄을 연다. WORKTABLE_EDITOR 로 바꿀 수 있다: {root} {file} {line} 이 채워진다.
/// 기본은 `code {root} -g {file}:{line}` — 워크트리 폴더를 같이 넘겨 그 워크트리 창에서 열리게 한다.
pub fn open_in_editor(reg: &Registry, key: &str, id: &str, line: usize) -> Result<Value, String> {
    let e = reg.entries.get(key).ok_or("세션이 없다")?;
    let x = find(reg, e, id).ok_or("항목이 없다")?;
    if !x.path.exists() {
        return Err(format!("파일이 지금은 없다: {}", x.rel_path()));
    }
    let root: PathBuf = x.root.clone().or_else(|| x.path.parent().map(Path::to_path_buf)).unwrap_or_default();
    let tpl = env::var("WORKTABLE_EDITOR").unwrap_or_else(|_| "code {root} -g {file}:{line}".into());
    let fill = |t: &str| {
        t.replace("{root}", &root.to_string_lossy())
            .replace("{file}", &x.path.to_string_lossy())
            .replace("{line}", &line.max(1).to_string())
    };
    let parts: Vec<String> = tpl.split_whitespace().map(fill).collect();
    let (cmd, args) = parts.split_first().ok_or("WORKTABLE_EDITOR 가 비었다")?;
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("{cmd} 실행 실패: {err}"))?;
    // 기다리지 않으면 좀비로 남는다 — 따로 거둔다.
    std::thread::spawn(move || child.wait());
    Ok(json!({"command": parts}))
}
