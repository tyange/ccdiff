//! 투두 파일을 칸 폭에 맞춰 그린다. 긴 줄은 들여 쓰며 줄을 바꾸고, 표가 폭을 넘으면 이슈마다 카드로 그린다.
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

fn w(s: &str) -> usize {
    Line::from(s).width()
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
        // room 칸이 찰 때까지.
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
        // 같은 스타일끼리 묶는다.
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

fn cells(line: &str) -> Vec<String> {
    line.trim().trim_matches('|').split('|').map(|c| c.trim().to_string()).collect()
}

pub fn is_table_sep(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn is_table(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// 표 줄들(start..end)을 그린다. 폭에 들어가면 열을 맞춘 표, 넘치면 첫 칸을 제목으로 한 카드.
fn table(lines: &[String], start: usize, end: usize, width: usize, out: &mut Vec<Row>) {
    let gray = Style::default().fg(Color::DarkGray);
    let rows: Vec<(usize, Vec<String>)> = (start..end).filter(|&i| !is_table_sep(&lines[i])).map(|i| (i, cells(&lines[i]))).collect();
    let mut cols: Vec<usize> = Vec::new();
    for (_, r) in &rows {
        for (i, c) in r.iter().enumerate() {
            let n = w(c);
            if cols.len() <= i { cols.push(n) } else { cols[i] = cols[i].max(n) }
        }
    }
    let total = cols.iter().sum::<usize>() + 3 * cols.len().saturating_sub(1);
    if total <= width {
        for (i, line) in lines.iter().enumerate().take(end).skip(start) {
            if is_table_sep(line) {
                out.push(Row { line: Line::from(Span::styled(cols.iter().map(|x| "─".repeat(*x)).collect::<Vec<_>>().join("─┼─"), gray)), src: Some(i) });
                continue;
            }
            let mut spans = Vec::new();
            for (k, c) in cells(line).iter().enumerate() {
                if k > 0 {
                    spans.push(Span::styled(" │ ", gray));
                }
                spans.extend(inline(c, Style::default()));
                spans.push(Span::raw(" ".repeat(cols.get(k).copied().unwrap_or(0).saturating_sub(w(c)))));
            }
            out.push(Row { line: Line::from(spans), src: Some(i) });
        }
        return;
    }
    // 카드: 첫 행은 머리(열 이름)로 쓴다. 카드 사이는 한 줄 띄운다.
    let Some(((_, head), body)) = rows.split_first() else { return };
    let label_w = head.iter().skip(1).map(|h| w(h)).max().unwrap_or(0);
    for (n, (i, r)) in body.iter().enumerate() {
        if n > 0 {
            out.push(Row { line: Line::default(), src: None });
        }
        let title = r.first().cloned().unwrap_or_default();
        let prefix = vec![Span::styled("▌ ", Style::default().fg(Color::Cyan))];
        for l in wrap(prefix, inline(&title, Style::default().add_modifier(Modifier::BOLD)), 2, width) {
            out.push(Row { line: l, src: Some(*i) });
        }
        for (k, c) in r.iter().enumerate().skip(1) {
            let name = head.get(k).cloned().unwrap_or_default();
            // "다음" 은 색을 달리해 상태와 가른다.
            let label_st = if name.contains("다음") { Style::default().fg(Color::Green) } else { gray };
            let pad = " ".repeat(label_w.saturating_sub(w(&name)));
            let base = 2 + label_w + 2;
            let parts = split_top(c);
            for (m, part) in parts.iter().enumerate() {
                let lead = if m == 0 {
                    vec![Span::raw("  "), Span::styled(format!("{name}{pad}  "), label_st)]
                } else {
                    vec![Span::raw(" ".repeat(base))]
                };
                // 여러 갈래면 갈래마다 · 를 붙여 줄바꿈과 구분한다.
                let (prefix, indent) = if parts.len() > 1 {
                    let mut p = lead;
                    p.push(Span::styled("· ", gray));
                    (p, base + 2)
                } else {
                    (lead, base)
                };
                for l in wrap(prefix, inline(part, Style::default()), indent, width) {
                    out.push(Row { line: l, src: Some(*i) });
                }
            }
        }
    }
}

/// 괄호 바깥의 ` · ` 에서 나눈다 — `PR ② 완료(tsc·eslint 0 · 회귀 24 passed)` 의 괄호 안은 그대로 둔다.
fn split_top(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut cur = String::new();
    let mut rest = s;
    while let Some(ch) = rest.chars().next() {
        if depth == 0 && rest.starts_with(" · ") {
            out.push(std::mem::take(&mut cur));
            rest = &rest[" · ".len()..];
            continue;
        }
        match ch {
            '(' | '[' | '{' | '「' | '（' => depth += 1,
            ')' | ']' | '}' | '」' | '）' => depth = (depth - 1).max(0),
            _ => {}
        }
        cur.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out.push(cur);
    out.retain(|p| !p.trim().is_empty());
    out
}

/// 섹션(`## 이름`) 안 항목 수. 투두는 남은 것, 표는 데이터 행, 나머지는 불릿.
fn section_count(lines: &[String], at: usize) -> Option<String> {
    let body: Vec<&String> = lines[at + 1..].iter().take_while(|l| !l.starts_with("## ") && !l.starts_with("# ")).collect();
    let tasks: Vec<bool> = body.iter().filter_map(|l| todo::task(l)).collect();
    if !tasks.is_empty() {
        let open = tasks.iter().filter(|d| !**d).count();
        return Some(format!("남은 {open} / {}", tasks.len()));
    }
    let rows = body.iter().filter(|l| is_table(l) && !is_table_sep(l)).count();
    if rows > 1 {
        return Some((rows - 1).to_string());
    }
    let bullets = body.iter().filter(|l| l.trim_start().starts_with("- ") || l.trim_start().starts_with("* ")).count();
    (bullets > 0).then(|| bullets.to_string())
}

pub fn render(lines: &[String], width: usize) -> Vec<Row> {
    let mut out = Vec::new();
    let mut i = 0;
    // 목록 항목(투두·불릿)이 이어지면 사이에 한 줄 띄운다.
    let mut prev_item = false;
    while i < lines.len() {
        let l = &lines[i];
        let item = todo::task(l).is_some() || l.trim_start().starts_with("- ") || l.trim_start().starts_with("* ");
        if item && prev_item {
            out.push(Row { line: Line::default(), src: None });
        }
        prev_item = item;
        if is_table(l) {
            let start = i;
            while i < lines.len() && is_table(&lines[i]) {
                i += 1;
            }
            table(lines, start, i, width, &mut out);
            continue;
        }
        let src = Some(i);
        if let Some(h) = l.strip_prefix("## ") {
            let head = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
            let mut spans = vec![Span::styled("▍", head), Span::styled(h.to_string(), head)];
            if let Some(n) = section_count(lines, i) {
                spans.push(Span::styled(format!(" · {n}"), Style::default().fg(Color::DarkGray)));
            }
            out.push(Row { line: Line::from(spans), src });
        } else if let Some(h) = l.strip_prefix("# ") {
            out.push(Row { line: Line::from(Span::styled(h.to_string(), Style::default().add_modifier(Modifier::BOLD))), src });
        } else if let Some(done) = todo::task(l) {
            let lead = l.len() - l.trim_start().len();
            let text = l.trim_start()[5..].trim_start();
            let (mark, mark_st, text_st) = if done {
                ("✓ ", Style::default().fg(Color::Green), Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT))
            } else {
                ("☐ ", Style::default().fg(Color::Yellow), Style::default())
            };
            let prefix = vec![Span::raw(" ".repeat(lead)), Span::styled(mark, mark_st)];
            for line in wrap(prefix, inline(text, text_st), lead + 2, width) {
                out.push(Row { line, src });
            }
        } else if let Some(rest) = l.trim_start().strip_prefix("- ").or_else(|| l.trim_start().strip_prefix("* ")) {
            let lead = l.len() - l.trim_start().len();
            let prefix = vec![Span::raw(" ".repeat(lead)), Span::styled("• ", Style::default().fg(Color::DarkGray))];
            for line in wrap(prefix, inline(rest, Style::default()), lead + 2, width) {
                out.push(Row { line, src });
            }
        } else if l.trim().is_empty() {
            out.push(Row { line: Line::default(), src });
        } else {
            for line in wrap(Vec::new(), inline(l, Style::default()), 0, width) {
                out.push(Row { line, src });
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_outside_parens() {
        assert_eq!(split_top("a · b(c · d) · e"), vec!["a", "b(c · d)", "e"]);
        assert_eq!(split_top("하나"), vec!["하나"]);
    }

    #[test]
    fn spaces_list_items_and_counts_sections() {
        let lines: Vec<String> = ["## 투두", "", "- [ ] 하나", "- [x] 둘"].iter().map(|s| s.to_string()).collect();
        let rows = render(&lines, 40);
        assert_eq!(text(&rows[0]), "▍투두 · 남은 1 / 2");
        // 제목, 빈 줄, 하나, 띄운 줄, 둘
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[3].src, None);
    }

    fn text(r: &Row) -> String {
        r.line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn styles_bold_and_code() {
        let s = inline("a **b** `c` d", Style::default());
        let t: Vec<&str> = s.iter().map(|x| x.content.as_ref()).collect();
        assert_eq!(t, vec!["a ", "b", " ", "c", " d"]);
        assert!(s[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn wraps_long_task_with_indent() {
        let rows = render(&["- [ ] 하나 둘 셋 넷 다섯 여섯 일곱 여덟 아홉 열".to_string()], 20);
        assert!(rows.len() > 1);
        assert!(rows.iter().all(|r| r.src == Some(0) && r.line.width() <= 20));
        assert!(text(&rows[1]).starts_with("  "));
    }

    #[test]
    fn wide_table_becomes_cards() {
        let lines: Vec<String> = ["| 이슈 | 상태 | 다음 |", "|---|---|---|", "| GH-1 | 아주 긴 상태 설명이 여기에 들어간다 | 머지 |"].iter().map(|s| s.to_string()).collect();
        let narrow = render(&lines, 24);
        assert_eq!(text(&narrow[0]), "▌ GH-1");
        assert!(narrow.iter().any(|r| text(r).starts_with("  상태  ")));
        assert!(narrow.iter().all(|r| r.src == Some(2)));
        let wide = render(&lines, 200);
        assert_eq!(wide.len(), 3);
    }
}
