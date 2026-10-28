//! 투두 칸 그리기 — 체크리스트 앱(Todoist·Things·미리 알림)의 흔한 모양을 터미널로 옮긴다.
//! 항목마다 동그라미 체크, 이슈 배지, 제목, 흐린 세부 줄(` — ` 뒤), 항목 사이 가는 구분선.
//! 남은 항목을 먼저, 완료한 항목은 아래 "완료" 묶음으로(파일 순서는 그대로). 아래에 특이사항.
//! 화면 줄마다 파일의 몇 번째 줄에서 나왔는지(src)를 같이 돌려준다 — 클릭·선택·스크롤이 이것을 쓴다.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::todo;

pub struct Row {
    pub line: Line<'static>,
    pub src: Option<usize>,
}

/// 체크 동그라미가 있는 칸(줄 맨 앞에서 0부터). 이 칸 근처를 누르면 체크한다.
pub const MARK_COL: usize = 3;
/// 제목 앞까지의 너비: 선택 막대·여백·동그라미·여백.
const LEAD: usize = 6;

fn w(s: &str) -> usize {
    Line::from(s).width()
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// `**굵게**`·`` `코드` `` 를 스타일로 바꾼다. 짝이 안 맞으면 남은 부분은 그대로 둔다.
fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let (mut bold, mut code) = (false, false);
    let mut buf = String::new();
    let style = |bold: bool, code: bool| {
        let mut s = base;
        if bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        if code {
            s = s.fg(Color::DarkGray);
        }
        s
    };
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("**") {
            if bold || r.contains("**") {
                out.push(Span::styled(std::mem::take(&mut buf), style(bold, code)));
                bold = !bold;
                rest = r;
                continue;
            }
        }
        if let Some(r) = rest.strip_prefix('`') {
            if code || r.contains('`') {
                out.push(Span::styled(std::mem::take(&mut buf), style(bold, code)));
                code = !code;
                rest = r;
                continue;
            }
        }
        let ch = rest.chars().next().expect("non-empty");
        buf.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out.push(Span::styled(buf, style(bold, code)));
    out.retain(|s| !s.content.is_empty());
    out
}

/// 첫 줄은 prefix 뒤에, 다음 줄부터는 indent 칸 들여 써서 width 에 맞춰 나눈다. 가능하면 공백에서 끊는다.
fn wrap(prefix: Vec<Span<'static>>, body: Vec<Span<'static>>, indent: usize, width: usize) -> Vec<Line<'static>> {
    let chars: Vec<(char, Style)> = body.iter().flat_map(|s| s.content.chars().map(move |c| (c, s.style))).collect();
    let mut lines = Vec::new();
    let mut i = 0;
    let mut first = true;
    loop {
        let lead = if first { prefix.iter().map(|s| w(&s.content)).sum() } else { indent };
        let room = width.saturating_sub(lead).max(8);
        let mut j = i;
        let mut used = 0;
        while j < chars.len() {
            let cw = w(&chars[j].0.to_string());
            if used + cw > room {
                break;
            }
            used += cw;
            j += 1;
        }
        if j < chars.len() {
            if let Some(k) = (i + 1..j).rev().find(|&k| chars[k].0 == ' ') {
                if k > i + (j - i) / 2 {
                    j = k + 1;
                }
            }
        }
        let mut spans = if first { prefix.clone() } else { vec![Span::raw(" ".repeat(indent))] };
        let mut k = i;
        while k < j {
            let st = chars[k].1;
            let mut s = String::new();
            while k < j && chars[k].1 == st {
                s.push(chars[k].0);
                k += 1;
            }
            spans.push(Span::styled(s, st));
        }
        lines.push(Line::from(spans));
        first = false;
        i = j;
        if i >= chars.len() {
            return lines;
        }
        while i < chars.len() && chars[i].0 == ' ' {
            i += 1;
        }
    }
}

/// 항목 앞의 이슈 번호(`#1234`·`GH-1234`)나 `(사용자)` 를 배지로 뗀다: (배지 글, 배지 색, 나머지).
fn badge(text: &str) -> Option<(String, Color, &str)> {
    let t = text.trim_start();
    if let Some(r) = t.strip_prefix("(사용자)") {
        return Some(("사용자".into(), Color::Magenta, r.trim_start()));
    }
    let num_part = t.strip_prefix('#').or_else(|| t.strip_prefix("GH-"))?;
    let digits: String = num_part.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    Some((format!("#{digits}"), Color::Blue, num_part[digits.len()..].trim_start()))
}

/// 괄호 바깥의 첫 ` — ` 에서 제목과 세부로 나눈다 — `계획(A — B) — 결정 후` 는 괄호 뒤에서.
fn split_dash(s: &str) -> Option<(&str, &str)> {
    let mut depth: i32 = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' | '[' | '{' | '「' | '（' => depth += 1,
            ')' | ']' | '}' | '」' | '）' => depth = (depth - 1).max(0),
            _ if depth == 0 && s[i..].starts_with(" — ") => return Some((&s[..i], &s[i + " — ".len()..])),
            _ => {}
        }
    }
    None
}

/// 섹션 몸통 줄 번호들(`## 이름` 다음부터 다음 `## ` 앞까지).
fn section_lines(lines: &[String], headings: &[&str]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut inside = false;
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("## ") || l.starts_with("# ") {
            inside = headings.contains(&l.trim_end());
            continue;
        }
        if inside {
            out.push(i);
        }
    }
    out
}

/// 화면 순서의 투두 줄 번호: 남은 것(파일 순서) 다음 완료한 것.
pub fn display_order(lines: &[String]) -> Vec<usize> {
    let tasks: Vec<usize> = section_lines(lines, &[todo::TODO_HEADING]).into_iter().filter(|&i| todo::task(&lines[i]).is_some()).collect();
    let (done, open): (Vec<usize>, Vec<usize>) = tasks.into_iter().partition(|&i| todo::task(&lines[i]) == Some(true));
    open.into_iter().chain(done).collect()
}

fn header(title: &str, count: String) -> Row {
    Row {
        line: Line::from(vec![
            Span::raw(" "),
            Span::styled(title.to_string(), Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {count}"), dim()),
        ]),
        src: None,
    }
}

fn rule(width: usize) -> Row {
    Row { line: Line::from(Span::styled(format!(" {}", "─".repeat(width.saturating_sub(2))), dim())), src: None }
}

/// 항목 하나: 선택 막대, 동그라미, 배지, 제목(` — ` 앞), 흐린 세부(뒤).
fn item(lines: &[String], i: usize, width: usize, selected: bool, out: &mut Vec<Row>) {
    let l = &lines[i];
    let done = todo::task(l) == Some(true);
    let text = l.trim_start()[5..].trim();
    let bar = if selected { Span::styled("▎", Style::default().fg(Color::Cyan)) } else { Span::raw(" ") };
    let mark = if done { Span::styled("✓", Style::default().fg(Color::Green)) } else { Span::styled("○", Style::default().fg(Color::Cyan)) };
    let mut prefix = vec![bar, Span::raw("  "), mark, Span::raw("  ")];
    // 앞에 이어진 배지를 모두 뗀다(`(사용자) #1036 …`).
    let mut rest = text;
    while let Some((b, color, r)) = badge(rest) {
        let st = if done { dim() } else { Style::default().fg(Color::White).bg(color).add_modifier(Modifier::BOLD) };
        prefix.push(Span::styled(format!(" {b} "), st));
        prefix.push(Span::raw(" "));
        rest = r;
    }
    let (title, detail) = match split_dash(rest) {
        Some((t, d)) => (t, Some(d)),
        None => (rest, None),
    };
    let title_st = match (done, selected) {
        (true, _) => dim().add_modifier(Modifier::CROSSED_OUT),
        (false, true) => Style::default().add_modifier(Modifier::BOLD),
        (false, false) => Style::default(),
    };
    for line in wrap(prefix, inline(title, title_st), LEAD, width) {
        out.push(Row { line, src: Some(i) });
    }
    // 완료한 항목은 제목만 — 세부는 접는다.
    if let Some(d) = detail.filter(|_| !done) {
        let lead = vec![Span::raw(" ".repeat(LEAD))];
        for line in wrap(lead, inline(d, dim()), LEAD, width) {
            out.push(Row { line, src: Some(i) });
        }
    }
}

/// sel 은 고른 투두 줄(칸에 포커스가 있을 때만 넘긴다 — 그때만 막대를 그린다).
pub fn render(lines: &[String], width: usize, sel: Option<usize>) -> Vec<Row> {
    let mut out = Vec::new();
    let order = display_order(lines);
    let open = order.iter().filter(|&&i| todo::task(&lines[i]) == Some(false)).count();
    out.push(Row { line: Line::default(), src: None });
    out.push(header("투두", format!("남은 {open}")));
    out.push(rule(width));
    let mut done_started = false;
    for &i in &order {
        if todo::task(&lines[i]) == Some(true) && !done_started {
            done_started = true;
            out.push(Row { line: Line::default(), src: None });
            out.push(header("완료", (order.len() - open).to_string()));
            out.push(rule(width));
        }
        item(lines, i, width, sel == Some(i), &mut out);
        out.push(rule(width));
    }
    if order.is_empty() {
        out.push(Row { line: Line::from(Span::styled("   a 로 추가", dim())), src: None });
    }
    // 특이사항: 불릿과 글 줄만(빈 줄은 건너뛴다). 없으면 섹션을 그리지 않는다.
    let notes: Vec<usize> = section_lines(lines, todo::NOTES_HEADINGS).into_iter().filter(|&i| !lines[i].trim().is_empty()).collect();
    if !notes.is_empty() {
        out.push(Row { line: Line::default(), src: None });
        out.push(header("특이사항", notes.len().to_string()));
        for i in notes {
            let l = lines[i].trim_start();
            let text = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).unwrap_or(l);
            let prefix = vec![Span::raw("   "), Span::styled("• ", Style::default().fg(Color::Yellow))];
            for line in wrap(prefix, inline(text, Style::default()), 5, width) {
                out.push(Row { line, src: Some(i) });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(r: &Row) -> String {
        r.line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn styles_bold_and_code() {
        let s = inline("a **b** `c` d", Style::default());
        let t: Vec<&str> = s.iter().map(|x| x.content.as_ref()).collect();
        assert_eq!(t, vec!["a ", "b", " ", "c", " d"]);
        assert!(s[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn badges() {
        assert_eq!(badge("#1036 로더").map(|b| (b.0, b.2)), Some(("#1036".into(), "로더")));
        assert_eq!(badge("GH-77 리뷰").map(|b| (b.0, b.2)), Some(("#77".into(), "리뷰")));
        assert_eq!(badge("(사용자) 원격 삭제").map(|b| (b.0, b.2)), Some(("사용자".into(), "원격 삭제")));
        assert!(badge("#번호 아님").is_none());
        assert!(badge("그냥 할 일").is_none());
        let l = lines(&["## 투두", "- [ ] (사용자) #9 지우기"]);
        let t: Vec<String> = render(&l, 60, None).iter().map(text).collect();
        assert!(t.iter().any(|x| x.contains(" 사용자   #9  지우기")), "{t:?}");
    }

    #[test]
    fn splits_title_outside_parens() {
        assert_eq!(split_dash("계획(A — B) — 결정 후"), Some(("계획(A — B)", "결정 후")));
        assert_eq!(split_dash("제목 — 세부 — 더"), Some(("제목", "세부 — 더")));
        assert_eq!(split_dash("나눌 것 없음"), None);
    }

    #[test]
    fn open_first_then_done() {
        let l = lines(&["## 투두", "- [x] 끝", "- [ ] 남1", "- [ ] 남2", "## 특이사항", "- 메모"]);
        assert_eq!(display_order(&l), vec![2, 3, 1]);
    }

    #[test]
    fn item_shape_and_mapping() {
        let l = lines(&["## 투두", "", "- [ ] #12 제목 — 세부 설명", "- [x] 끝난 일", "", "## 특이사항", "- 조심할 점"]);
        let rows = render(&l, 60, Some(2));
        let all: Vec<String> = rows.iter().map(text).collect();
        assert!(all.iter().any(|t| t.starts_with("▎  ○   #12  제목")), "{all:?}");
        assert!(all.iter().any(|t| t.trim() == "세부 설명"));
        assert!(all.iter().any(|t| t.contains("완료")));
        assert!(all.iter().any(|t| t.contains("특이사항")));
        // 항목 줄은 그 투두 줄을, 구분선·머리줄은 None 을 가리킨다.
        assert!(rows.iter().filter(|r| text(r).contains("제목")).all(|r| r.src == Some(2)));
        assert!(rows.iter().filter(|r| text(r).contains('─')).all(|r| r.src.is_none()));
        let first = all.iter().find(|t| t.contains("제목")).unwrap();
        assert_eq!(first.chars().nth(MARK_COL), Some('○'));
    }

    #[test]
    fn wraps_long_title_with_indent() {
        let l = lines(&["## 투두", "- [ ] 하나 둘 셋 넷 다섯 여섯 일곱 여덟 아홉 열 열하나"]);
        let rows = render(&l, 24, None);
        let item: Vec<&Row> = rows.iter().filter(|r| r.src == Some(1)).collect();
        assert!(item.len() > 1);
        assert!(item.iter().all(|r| r.line.width() <= 24));
        assert!(text(item[1]).starts_with("      "));
    }
}
