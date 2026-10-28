use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use ccdiff_core::git::CommitState;

use crate::{
    replay::Phase,
    rows::{compact, Row, RowKind},
    App, Disp, WT,
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

/// 고른 줄은 줄 번호 칸을 뒤집어 보인다 — 코드 색과 diff 배경은 그대로 둔다.
fn mark_selected(mut line: Line<'static>, selected: bool) -> Line<'static> {
    if selected {
        if let Some(first) = line.spans.first_mut() {
            first.style = Style::default().fg(Color::Black).bg(Color::Yellow);
        }
    }
    line
}

fn is_selected(app: &App, key: usize, j: usize) -> bool {
    app.sel.as_ref().is_some_and(|s| {
        let (a, b) = s.range();
        s.key == key && (a..=b).contains(&j)
    })
}

/// 상태줄 앞에 붙는 IDE 연결 표시.
fn ide_label(app: &App) -> String {
    let Some(ide) = &app.ide else { return String::new() };
    let conn = match ide.connected() {
        0 => format!("IDE :{} 대기", ide.port),
        n => format!("IDE 연결 {n}"),
    };
    match app.sent {
        Some(n) => format!(" ⧉ {n}줄 → Claude · {conn} ·"),
        None => format!(" {conn} ·"),
    }
}

/// 목록 줄 앞 표시와 제목 꼬리.
fn commit_mark(app: &App, key: usize) -> (Span<'static>, String) {
    if key >= WT {
        return (
            Span::styled("◆ ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            " · ◆ 작업 트리(HEAD 대비, Bash 수정 포함)".to_string(),
        );
    }
    match app.commits.get(&key) {
        Some(CommitState::Yes { hash, subject }) => (
            Span::styled("✓ ", Style::default().fg(Color::Green)),
            format!(" · ✓ {hash} {subject}"),
        ),
        Some(CommitState::Changed { hash, subject }) => (
            Span::styled("◐ ", Style::default().fg(Color::Cyan)),
            format!(" · ◐ {hash} {subject} (커밋 전에 더 고침)"),
        ),
        Some(CommitState::Gone) => (Span::styled("✗ ", gray()), " · ✗ 커밋 없이 지워진 파일".to_string()),
        Some(CommitState::No) => (
            Span::styled("● ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            " · ● 미커밋".to_string(),
        ),
        _ => (Span::raw("  "), String::new()),
    }
}

/// 목록에 이름이 겹치는 파일이 있으면 상위 폴더까지 붙인다(`ko/lang.json`).
fn display_name(e: &ccdiff_core::session::EditEntry, count: &std::collections::HashMap<String, usize>) -> String {
    let name = e.file_name();
    if count.get(&name).copied().unwrap_or(0) < 2 {
        return name;
    }
    let parent = e.path.parent().and_then(|p| p.file_name()).map(|p| p.to_string_lossy().into_owned());
    match parent {
        Some(p) => format!("{p}/{name}"),
        None => name,
    }
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
    let ide = ide_label(app);
    let text = if app.file.is_some() {
        draw_file(f, app, main);
        let state = match app.file.as_ref().and_then(|v| v.replay.as_ref()) {
            Some(r) if r.paused => "⏸ 일시정지",
            Some(_) => "▶ 재생 중",
            None => "파일 보기",
        };
        format!(
            "{ide} {state} · {theme} │ v 줄 고르기(j/k 늘이기·드래그) · e IDE 로 열기 · n/p 다음·이전 수정 · r 처음부터 재생 · space 일시정지 · j/k·PgUp/PgDn 스크롤 · g/G 처음/끝 · Esc 목록 · t 테마 · b 밝기"
        )
    } else {
        draw_list(f, app, main);
        let follow = if app.follow { "따라가기 켜짐" } else { "따라가기 꺼짐" };
        format!(
            "{ide} {follow} · {theme} │ 드래그 줄 고르기 → Claude · e IDE 로 열기 · Enter 파일 보기·그룹 접기 · z/Z 그룹 접기/전부 · j/k 이동 · J/K·PgUp/PgDn 스크롤 · ←/→ 가로 · g/G 처음/끝 · f 따라가기 · w 워크트리 · c 커밋 필터 · a 세션 전 변경 · h git 밖 · t 테마 · b 밝기 · q 종료"
        )
    };
    let text = match &app.notice {
        Some((m, at)) if at.elapsed().as_secs() < 4 => format!(" {m}"),
        _ => text,
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
    let name_width = (left.width as usize).saturating_sub(35);
    let mut name_count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for &i in &visible {
        *name_count.entry(app.entry(i).file_name()).or_default() += 1;
    }
    let display = app.display();
    let items: Vec<ListItem> = display
        .iter()
        .map(|d| {
            let i = match d {
                Disp::Item(k) => *k,
                Disp::Header { key, count, collapsed } => {
                    let e = app.entry(*key);
                    let path = e.root.as_ref().map(|r| r.display().to_string()).unwrap_or_else(|| "(git 밖)".into());
                    let arrow = if *collapsed { "▸" } else { "▾" };
                    let root = &e.root;
                    let latest = visible.iter().filter(|&&k| &app.entry(k).root == root).map(|&k| app.entry(k)).max_by_key(|x| x.epoch);
                    let when = latest
                        .map(|x| {
                            let day = x.time.get(0..5).unwrap_or("");
                            let hm = x.time.get(6..11).unwrap_or("");
                            let d = if day == today_md { "오늘" } else if day == yday_md { "어제" } else { day };
                            format!("최근 {d} {hm} · ")
                        })
                        .unwrap_or_default();
                    return ListItem::new(Line::from(vec![
                        Span::styled(format!("{arrow} {} ", e.label()), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                        Span::raw(format!("{count}건 · ")),
                        Span::styled(format!("{when}{path}"), gray()),
                    ]));
                }
            };
            let e = app.entry(i);
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
            let (mark, _) = commit_mark(app, i);
            // 커밋된 수정은 흐리게 — 아직 리뷰할 게 남은 미커밋 쪽으로 눈이 가게 한다.
            let dim = if app.is_committed(i) { gray() } else { Style::default() };
            ListItem::new(Line::from(vec![
                mark,
                Span::styled(format!("{day_label} "), day_style),
                Span::styled(format!("{hm} "), dim),
                Span::styled(format!("{:<7}", e.kind), dim),
                Span::styled(format!("+{:<4}", e.added), Style::default().fg(Color::Green)),
                Span::styled(format!("-{:<4} ", e.removed), Style::default().fg(Color::Red)),
                Span::styled(truncate_left(&display_name(e, &name_count), name_width), dim),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title({
            let cf = app.commit_filter_label();
            let cf = if cf.is_empty() { String::new() } else { format!(" · {cf}") };
            format!(" 수정 {}건 · {}{cf} · 미커밋 {} ", visible.len(), app.filter_label(), app.uncommitted_count())
        }))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, left, &mut app.list);
    app.list_area = left;
    app.diff_area = right;

    let sel = app.selected_entry();
    let title_width = (right.width as usize).saturating_sub(4);
    let mut clamped = None;
    let mut diff_rows = Vec::new();
    let (title, body) = match sel {
        Some(i) => {
            app.ensure_rows(i);
            let e = app.entry(i);
            let rows = &app.rows[&i];
            let bgs = app.hl.diff_bgs();
            let mut body = Vec::new();
            if e.user_modified {
                body.push(Line::from(Span::styled(
                    "사용자가 승인 전에 손으로 고친 수정",
                    Style::default().fg(Color::Magenta),
                )));
            }
            let plan = compact(rows);
            let height = right.height.saturating_sub(2) as usize;
            let total = body.len() + plan.len();
            let sc = app.scroll.min(total.saturating_sub(height) as u16);
            clamped = Some(sc);
            // 보이는 높이만큼만 그린다 — 1만 줄짜리 새 파일도 매 프레임 전부 만들지 않게.
            let skip = (sc as usize).saturating_sub(body.len());
            body = body.into_iter().skip(sc as usize).collect();
            let room = height.saturating_sub(body.len());
            let mut map: Vec<Option<(usize, usize)>> = vec![None; body.len()];
            for o in plan.into_iter().skip(skip).take(room) {
                map.push(o.map(|j| (i, j)));
                body.push(match o {
                    Some(j) => mark_selected(row_line(&rows[j], bgs, None, false), is_selected(app, i, j)),
                    None => sep_line(),
                });
            }
            diff_rows = map;
            if total == 0 {
                body.push(Line::from("(diff 없음)"));
            }
            let (_, tail) = commit_mark(app, i);
            let head = format!(" {} · {} · ", e.time, e.label());
            let room = title_width.saturating_sub(head.chars().count() + tail.chars().count() + 1);
            (format!("{head}{}{tail} ", truncate_left(&e.rel_path(), room)), body)
        }
        None if app.on_header() => (
            format!(" {} ", app.session),
            vec![Line::from(Span::styled("워크트리 머리줄 — Enter·space·클릭으로 접기/펴기, Z 로 전부", gray()))],
        ),
        None => (format!(" {} ", app.session), vec![Line::from("아직 수정이 없다")]),
    };
    let diff = Paragraph::new(body)
        .block(Block::default().borders(Borders::ALL).title(truncate_left(&title, title_width)))
        .scroll((0, app.hscroll));
    f.render_widget(diff, right);
    if let Some(sc) = clamped {
        app.scroll = sc;
    }
    app.diff_rows = diff_rows;
}

fn draw_file(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(idx) = app.file.as_ref().map(|v| v.seq[v.pos]) else { return };
    app.ensure_rows(idx);
    app.diff_area = area;
    let height = area.height.saturating_sub(2) as usize;

    // 그릴 줄 계획(줄 번호, 보일 글자 수, 커서)만 먼저 세우고, 화면에 보이는 높이만큼만 실제로 그린다.
    let (lines, focus, first_change) = {
        let fv = app.file.as_ref().expect("file view");
        let rows = &app.rows[&idx];
        let mut lines: Vec<(usize, Option<usize>, bool)> = Vec::new();
        let mut focus = None;
        let mut first_change = None;
        match &fv.replay {
            None => {
                for (i, r) in rows.iter().enumerate() {
                    if r.is_change() && first_change.is_none() {
                        first_change = Some(lines.len());
                    }
                    lines.push((i, None, false));
                }
            }
            Some(rp) => {
                let mut budget = rp.typed;
                let mut typing_done = false;
                for (i, r) in rows.iter().enumerate() {
                    match (rp.phase, r.kind) {
                        (Phase::Delete, RowKind::Add) => {}
                        (Phase::Delete, RowKind::Del) => {
                            focus.get_or_insert(lines.len());
                            lines.push((i, None, false));
                        }
                        (_, RowKind::Del) => {}
                        (Phase::Type, RowKind::Add) => {
                            if typing_done {
                                continue;
                            }
                            let w = r.width();
                            if budget > w {
                                budget -= w + 1;
                                lines.push((i, None, false));
                            } else {
                                focus = Some(lines.len());
                                lines.push((i, Some(budget), true));
                                typing_done = true;
                            }
                        }
                        (Phase::Hold, RowKind::Add) => {
                            focus.get_or_insert(lines.len());
                            lines.push((i, None, false));
                        }
                        _ => lines.push((i, None, false)),
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
    let rows = &app.rows[&idx];
    let bgs = app.hl.diff_bgs();
    let shown: Vec<&(usize, Option<usize>, bool)> = lines.iter().skip(scroll as usize).take(height).collect();
    // 재생 중에는 줄이 계속 바뀌어 고를 수 없다.
    let replaying = app.file.as_ref().is_some_and(|v| v.replay.is_some());
    let map = shown.iter().map(|&&(i, _, _)| if replaying { None } else { Some((idx, i)) }).collect();
    let lines: Vec<Line> = shown
        .iter()
        .map(|&&(i, reveal, cursor)| mark_selected(row_line(&rows[i], bgs, reveal, cursor), is_selected(app, idx, i)))
        .collect();
    app.diff_rows = map;

    let e = app.entry(idx);
    let title_width = (area.width as usize).saturating_sub(4);
    let (_, tail) = commit_mark(app, idx);
    let head = format!(" 수정 {}/{} · {} · {} · ", pos + 1, n, e.time, e.label());
    let room = title_width.saturating_sub(head.chars().count() + tail.chars().count() + 1);
    let title = format!("{head}{}{tail} ", truncate_left(&e.rel_path(), room));
    let view = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .scroll((0, app.hscroll));
    f.render_widget(view, area);
}
