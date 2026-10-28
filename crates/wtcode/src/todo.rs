//! 날짜별 투두 파일. 사람(wtcode)과 에이전트(Claude)가 같은 마크다운 파일을 고친다.
//! 위치: WTCODE_TODO_DIR, 없으면 $XDG_DATA_HOME/wtcode/todo, 없으면 ~/.local/share/wtcode/todo — `YYYY-MM-DD.md`.
//! 형식: `# 작업 현황·투두 — 날짜`, `## 끝난 것`, `## 진행 중`(| 이슈 | 상태 | 다음 | 표), `## 투두`(- [ ] / - [x]), `## 메모`.
//! 오늘 파일이 없으면 가장 최근 파일에서 진행 중 표와 끝나지 않은 투두만 옮겨 만든다.
//! 고칠 때는 파일을 다시 읽고 그 줄이 화면에 보인 것과 같을 때만 바꾼다 — 에이전트가 그 사이 고쳤을 수 있다.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

pub const TODO_HEADING: &str = "## 투두";
const DOING_HEADING: &str = "## 진행 중";

pub fn dir() -> PathBuf {
    if let Some(d) = env::var_os("WTCODE_TODO_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(d);
    }
    let data = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    data.join("wtcode/todo")
}

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn skeleton(date: &str, doing: &[String], todos: &[String]) -> String {
    let mut s = format!("# 작업 현황·투두 — {date}\n\n## 끝난 것\n\n{DOING_HEADING}\n\n");
    if doing.is_empty() {
        s += "| 이슈 | 상태 | 다음 |\n|---|---|---|\n";
    } else {
        for l in doing {
            s += l;
            s += "\n";
        }
    }
    s += &format!("\n{TODO_HEADING}\n\n");
    for l in todos {
        s += l;
        s += "\n";
    }
    s += "\n## 메모\n";
    s
}

/// 한 섹션(`## 이름` 다음부터 다음 `## ` 앞까지)의 줄들.
fn section<'a>(content: &'a str, heading: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut inside = false;
    for l in content.lines() {
        if l.starts_with("## ") || l.starts_with("# ") {
            inside = l.trim_end() == heading;
            continue;
        }
        if inside {
            out.push(l);
        }
    }
    out
}

/// 오늘 파일 경로. 없으면 가장 최근 파일에서 진행 중 표와 끝나지 않은 투두를 옮겨 만든다.
pub fn ensure_today() -> io::Result<PathBuf> {
    let d = dir();
    fs::create_dir_all(&d)?;
    let date = today();
    let path = d.join(format!("{date}.md"));
    if path.exists() {
        return Ok(path);
    }
    let mut prev: Vec<PathBuf> = fs::read_dir(&d)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md") && p.file_stem().is_some_and(|s| s.to_string_lossy().as_ref() < date.as_str()))
        .collect();
    prev.sort();
    let (doing, todos) = match prev.last().and_then(|p| fs::read_to_string(p).ok()) {
        Some(c) => (
            section(&c, DOING_HEADING).into_iter().filter(|l| l.trim_start().starts_with('|')).map(String::from).collect(),
            section(&c, TODO_HEADING).into_iter().filter(|l| task(l) == Some(false)).map(String::from).collect(),
        ),
        None => (Vec::new(), Vec::new()),
    };
    write(&path, &skeleton(&date, &doing, &todos))?;
    Ok(path)
}

/// 체크박스 줄이면 체크 여부. `- [ ]`·`- [x]`(* · + 도 받는다).
pub fn task(line: &str) -> Option<bool> {
    let t = line.trim_start();
    let rest = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ "))?;
    match rest.get(..3)? {
        "[ ]" => Some(false),
        "[x]" | "[X]" => Some(true),
        _ => None,
    }
}

/// 다른 쪽이 반쯤 쓴 파일을 읽지 않게 임시 파일에 쓰고 바꿔 끼운다.
fn write(path: &Path, content: &str) -> io::Result<()> {
    let tmp = path.with_extension("md.tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)
}

/// 줄 하나를 바꾼다. 그 줄이 expect 와 다르면(그 사이 다른 쪽이 고쳤으면) 손대지 않는다.
fn edit_line(path: &Path, at: usize, expect: &str, f: impl FnOnce(&str) -> Option<String>) -> Result<(), String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    if lines.get(at).map(String::as_str) != Some(expect) {
        return Err("그 사이 파일이 바뀌었다 — 다시 읽었으니 다시 시도".into());
    }
    match f(&lines[at]) {
        Some(new) => lines[at] = new,
        None => {
            lines.remove(at);
        }
    }
    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    write(path, &out).map_err(|e| e.to_string())
}

pub fn toggle(path: &Path, at: usize, expect: &str) -> Result<(), String> {
    edit_line(path, at, expect, |l| {
        let i = l.find('[')?;
        let mark = if task(l)? { ' ' } else { 'x' };
        Some(format!("{}[{mark}]{}", &l[..i], &l[i + 3..]))
    })
}

pub fn delete(path: &Path, at: usize, expect: &str) -> Result<(), String> {
    edit_line(path, at, expect, |_| None)
}

/// `## 투두` 섹션 끝(다음 섹션 앞 빈 줄 전)에 `- [ ] text` 를 더한다. 섹션이 없으면 만든다.
pub fn add(path: &Path, text: &str) -> Result<(), String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let item = format!("- [ ] {}", text.trim());
    match lines.iter().position(|l| l.trim_end() == TODO_HEADING) {
        Some(h) => {
            let end = lines[h + 1..].iter().position(|l| l.starts_with("## ")).map(|i| h + 1 + i).unwrap_or(lines.len());
            // 섹션 끝의 빈 줄 앞에 넣는다.
            let mut at = end;
            while at > h + 1 && lines[at - 1].trim().is_empty() {
                at -= 1;
            }
            if at == h + 1 {
                // 비어 있는 섹션: 제목 다음 빈 줄 하나 두고.
                lines.insert(h + 1, String::new());
                at = h + 2;
            }
            lines.insert(at, item);
        }
        None => {
            lines.extend([String::new(), TODO_HEADING.to_string(), String::new(), item]);
        }
    }
    write(path, &(lines.join("\n") + "\n")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, content: &str) -> PathBuf {
        let d = env::temp_dir().join(format!("wtcode-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let p = d.join("t.md");
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn detects_tasks() {
        assert_eq!(task("- [ ] a"), Some(false));
        assert_eq!(task("  - [x] b"), Some(true));
        assert_eq!(task("* [X] c"), Some(true));
        assert_eq!(task("- a"), None);
        assert_eq!(task("| GH-1 | x |"), None);
    }

    #[test]
    fn toggles_and_refuses_stale_edit() {
        let p = tmp("toggle", "## 투두\n\n- [ ] 하나\n- [x] 둘\n");
        toggle(&p, 2, "- [ ] 하나").unwrap();
        toggle(&p, 3, "- [x] 둘").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "## 투두\n\n- [x] 하나\n- [ ] 둘\n");
        assert!(toggle(&p, 2, "- [ ] 하나").is_err());
    }

    #[test]
    fn adds_at_end_of_todo_section() {
        let p = tmp("add", "# t\n\n## 투두\n\n- [ ] 하나\n\n## 메모\n- m\n");
        add(&p, "둘").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "# t\n\n## 투두\n\n- [ ] 하나\n- [ ] 둘\n\n## 메모\n- m\n");
        let q = tmp("add-empty", "# t\n\n## 투두\n\n## 메모\n");
        add(&q, "첫").unwrap();
        assert_eq!(fs::read_to_string(&q).unwrap(), "# t\n\n## 투두\n\n- [ ] 첫\n\n## 메모\n");
    }

    #[test]
    fn rolls_over_doing_and_open_todos() {
        let prev = "# 작업 현황·투두 — 2000-01-01\n\n## 끝난 것\n- 끝\n\n## 진행 중\n\n| 이슈 | 상태 | 다음 |\n|---|---|---|\n| GH-1 | 리뷰 | 머지 |\n\n## 투두\n\n- [x] 했다\n- [ ] 남았다\n\n## 메모\n- 메모\n";
        let s = section(prev, DOING_HEADING);
        assert!(s.contains(&"| GH-1 | 리뷰 | 머지 |"));
        let doing: Vec<String> = s.into_iter().filter(|l| l.trim_start().starts_with('|')).map(String::from).collect();
        let todos: Vec<String> = section(prev, TODO_HEADING).into_iter().filter(|l| task(l) == Some(false)).map(String::from).collect();
        let out = skeleton("2000-01-02", &doing, &todos);
        assert!(out.contains("| GH-1 | 리뷰 | 머지 |"));
        assert!(out.contains("- [ ] 남았다"));
        assert!(!out.contains("했다"));
        assert!(!out.contains("- 끝"));
    }
}
