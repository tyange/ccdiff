use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::{
    replay::Phase,
    rows::{compact, Row, RowKind},
    App,
};

/// 바뀐 줄 배경을 줄 끝까지 칠하려고 붙이는 공백 수.
const PAD: usize = 400;

fn gray() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn sep_line() -> Line<'static> {
    Line::from(Span::styled("            ⋯", gray()))
}

/// reveal = 보여 줄 글자 수(재생 중 타이핑), cursor = 끝에 커서 표시.
fn row_line(r: &Row, bgs: (Color, Color), reveal: Option<usize>, cursor: bool) -> Line<'static> {
    if r.kind == RowKind::Sep {
        return sep_line();
    }
    let (bg, mark, mark_fg) = match r.kind {
        RowKind::Add => (Some(bgs.0), '+', Color::Green),
        RowKind::Del => (Some(bgs.1), '-', Color::Red),
        _ => (None, ' ', Color::DarkGray),
    };
    let st = |s: Style| match bg {
        Some(c) => s.bg(c),
        None => s,
    };
    let o = r.old_no.map(|n| n.to_string()).unwrap_or_default();
    let n = r.new_no.map(|n| n.to_string()).unwrap_or_default();
    let mut spans = vec![
        Span::styled(format!("{o:>5} {n:>5} "), gray()),
        Span::styled(format!("{mark} "), st(Style::default().fg(mark_fg))),
    ];
    let mut budget = reveal.unwrap_or(usize::MAX);
    let mut used = 0;
    for (c, t) in &r.spans {
        if budget == 0 {
            break;
        }
        let part: String = t.chars().take(budget).collect();
        let k = part.chars().count();
        budget -= k;
        used += k;
        spans.push(Span::styled(part, st(Style::default().fg(*c))));
    }
    if cursor {
        spans.push(Span::styled("▌", st(Style::default().fg(Color::Yellow))));
        used += 1;
    }
    if bg.is_some() {
        spans.push(Span::styled(" ".repeat(PAD.saturating_sub(used)), st(Style::default())));
    }
    Line::from(spans)
}

fn truncate_left(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width || width < 2 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - (width - 1)).collect();
    format!("…{tail}")
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let theme = app.hl.theme_name();
    let text = if app.file.is_some() {
        draw_file(f, app, main);
        let state = match app.file.as_ref().and_then(|v| v.replay.as_ref()) {
            Some(r) if r.paused => "⏸ 일시정지",
            Some(_) => "▶ 재생 중",
            None => "파일 보기",
        };
        format!(
            " {state} · {theme} │ n/p 다음·이전 수정 · r 처음부터 재생 · space 일시정지 · j/k·PgUp/PgDn 스크롤 · g/G 처음/끝 · Esc 목록 · t 테마 · b 밝기"
        )
    } else {
        draw_list(f, app, main);
        let follow = if app.follow { "따라가기 켜짐" } else { "따라가기 꺼짐" };
        format!(
            " {follow} · {theme} │ Enter 파일 보기 · j/k 이동 · J/K·PgUp/PgDn 스크롤 · ←/→ 가로 · g/G 처음/끝 · f 따라가기 · w 워크트리 · h git 밖 · t 테마 · b 밝기 · q 종료"
        )
    };
    f.render_widget(Paragraph::new(text).style(gray()), status);
}

fn draw_list(f: &mut Frame, app: &mut App, main: Rect) {
    // 좁은 pane(tmux 반쪽 등)에서는 목록을 위, diff 를 아래로 쌓는다.
    let [left, right] = if main.width < 140 {
        Layout::vertical([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(main)
    } else {
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(main)
    };

    let visible = app.visible();
    let today = chrono::Local::now().date_naive();
    let md = |d: chrono::NaiveDate| d.format("%m-%d").to_string();
    let (today_md, yday_md) = (md(today), today.pred_opt().map(md).unwrap_or_default());
    let name_width = (left.width as usize).saturating_sub(42);
    let items: Vec<ListItem> = visible
        .iter()
        .map(|&i| {
            let e = &app.entries[i];
            let hm = e.time.get(6..11).unwrap_or(&e.time);
            let day = e.time.get(0..5).unwrap_or("");
            // 한글 두 글자는 4칸이라 공백 하나를 더해 `10-27`(5칸)과 폭을 맞춘다.
            let (day_label, day_style) = if day == today_md {
                ("오늘 ".to_string(), Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
            } else if day == yday_md {
                ("어제 ".to_string(), Style::default().fg(Color::Magenta))
            } else {
                (day.to_string(), gray())
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{day_label} "), day_style),
                Span::raw(format!("{hm} ")),
                Span::raw(format!("{:<7}", e.kind)),
                Span::styled(format!("+{:<4}", e.added), Style::default().fg(Color::Green)),
                Span::styled(format!("-{:<4} ", e.removed), Style::default().fg(Color::Red)),
                Span::styled(format!("{:<8} ", e.label()), Style::default().fg(Color::Yellow)),
                Span::raw(truncate_left(&e.file_name(), name_width)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " 수정 {}건 · {} ",
            visible.len(),
            app.filter_label()
        )))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, left, &mut app.list);
    app.list_area = left;
    app.diff_area = right;

    let sel = app.list.selected().and_then(|s| visible.get(s)).copied();
    let title_width = (right.width as usize).saturating_sub(4);
    let (title, body) = match sel {
        Some(i) => {
            app.ensure_rows(i);
            let e = &app.entries[i];
            let rows = &app.rows[&i];
            let bgs = app.hl.diff_bgs();
            let mut body = Vec::new();
            if e.user_modified {
                body.push(Line::from(Span::styled(
                    "사용자가 승인 전에 손으로 고친 수정",
                    Style::default().fg(Color::Magenta),
                )));
            }
            body.extend(compact(rows).into_iter().map(|o| match o {
                Some(j) => row_line(&rows[j], bgs, None, false),
                None => sep_line(),
            }));
            if body.is_empty() {
                body.push(Line::from("(diff 없음)"));
            }
            let head = format!(" {} · {} · ", e.time, e.label());
            let room = title_width.saturating_sub(head.chars().count() + 1);
            (format!("{head}{} ", truncate_left(&e.rel_path(), room)), body)
        }
        None => (format!(" {} ", app.session), vec![Line::from("아직 수정이 없다")]),
    };
    let diff = Paragraph::new(body)
        .block(Block::default().borders(Borders::ALL).title(truncate_left(&title, title_width)))
        .scroll((app.scroll, app.hscroll));
    f.render_widget(diff, right);
}

fn draw_file(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(idx) = app.file.as_ref().map(|v| v.seq[v.pos]) else { return };
    app.ensure_rows(idx);
    app.diff_area = area;
    let height = area.height.saturating_sub(2) as usize;

    let (lines, focus, first_change) = {
        let fv = app.file.as_ref().expect("file view");
        let rows = &app.rows[&idx];
        let bgs = app.hl.diff_bgs();
        let mut lines = Vec::new();
        let mut focus = None;
        let mut first_change = None;
        match &fv.replay {
            None => {
                for r in rows {
                    if r.is_change() && first_change.is_none() {
                        first_change = Some(lines.len());
                    }
                    lines.push(row_line(r, bgs, None, false));
                }
            }
            Some(rp) => {
                let mut budget = rp.typed;
                let mut typing_done = false;
                for r in rows {
                    match (rp.phase, r.kind) {
                        (Phase::Delete, RowKind::Add) => {}
                        (Phase::Delete, RowKind::Del) => {
                            focus.get_or_insert(lines.len());
                            lines.push(row_line(r, bgs, None, false));
                        }
                        (_, RowKind::Del) => {}
                        (Phase::Type, RowKind::Add) => {
                            if typing_done {
                                continue;
                            }
                            let w = r.width();
                            if budget > w {
                                budget -= w + 1;
                                lines.push(row_line(r, bgs, None, false));
                            } else {
                                focus = Some(lines.len());
                                lines.push(row_line(r, bgs, Some(budget), true));
                                typing_done = true;
                            }
                        }
                        (Phase::Hold, RowKind::Add) => {
                            focus.get_or_insert(lines.len());
                            lines.push(row_line(r, bgs, None, false));
                        }
                        _ => lines.push(row_line(r, bgs, None, false)),
                    }
                }
            }
        }
        (lines, focus, first_change)
    };

    let max_scroll = lines.len().saturating_sub(height) as u16;
    let fv = app.file.as_mut().expect("file view");
    if fv.replay.is_some() {
        if let Some(fc) = focus {
            fv.scroll = fc.saturating_sub(height / 3) as u16;
        }
    } else if fv.jump {
        fv.scroll = first_change.unwrap_or(0).saturating_sub(5) as u16;
        fv.jump = false;
    }
    fv.scroll = fv.scroll.min(max_scroll);
    let (scroll, pos, n) = (fv.scroll, fv.pos, fv.seq.len());

    let e = &app.entries[idx];
    let title_width = (area.width as usize).saturating_sub(4);
    let head = format!(" 수정 {}/{} · {} · {} · ", pos + 1, n, e.time, e.label());
    let room = title_width.saturating_sub(head.chars().count() + 1);
    let title = format!("{head}{} ", truncate_left(&e.rel_path(), room));
    let view = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .scroll((scroll, app.hscroll));
    f.render_widget(view, area);
}
