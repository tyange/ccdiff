//! 수정 하나를 "파일 전체 기준 줄 목록"으로 펼친다. 화면·재생이 모두 이 목록을 쓴다.

use crate::{
    highlight::{Highlighter, Spans},
    session::EditEntry,
};

const MAX_LINES: usize = 20_000;
pub const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Ctx,
    Add,
    Del,
    /// 생략 구간 표시(diff 조각만 있을 때 조각 사이).
    Sep,
}

pub struct Row {
    pub kind: RowKind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub spans: Spans,
}

impl Row {
    pub fn width(&self) -> usize {
        self.spans.iter().map(|(_, t)| t.chars().count()).sum()
    }

    pub fn is_change(&self) -> bool {
        matches!(self.kind, RowKind::Add | RowKind::Del)
    }
}

enum Src {
    Before(usize),
    After(usize),
}

/// (종류, 옛 줄 번호, 새 줄 번호, 색을 가져올 곳)
type Plan = (RowKind, Option<usize>, Option<usize>, Src);

fn push_ctx(bi: usize, text: String, after: &mut Vec<String>, plan: &mut Vec<Plan>) {
    after.push(text);
    plan.push((RowKind::Ctx, Some(bi + 1), Some(after.len()), Src::After(after.len() - 1)));
}

pub fn build_rows(e: &EditEntry, hl: &Highlighter) -> Vec<Row> {
    let before: Vec<String> = match (&e.before, e.kind) {
        (Some(b), _) => b.lines().take(MAX_LINES).map(String::from).collect(),
        (None, "Write+") => Vec::new(),
        (None, _) => return fragment_rows(e, hl),
    };

    let mut after: Vec<String> = Vec::new();
    let mut plan: Vec<Plan> = Vec::new();

    if e.hunks.is_empty() {
        if e.kind == "Write+" {
            for l in e.content.as_deref().unwrap_or("").lines().take(MAX_LINES) {
                after.push(l.to_string());
                plan.push((RowKind::Add, None, Some(after.len()), Src::After(after.len() - 1)));
            }
        } else {
            for (bi, l) in before.iter().enumerate() {
                push_ctx(bi, l.clone(), &mut after, &mut plan);
            }
        }
    } else {
        let mut bi = 0;
        for h in &e.hunks {
            // oldLines 가 0 이면 oldStart 는 "이 줄 뒤에 넣는다" 는 뜻이다.
            let start = if h.old_lines == 0 { h.old_start } else { h.old_start.saturating_sub(1) };
            while bi < start && bi < before.len() {
                push_ctx(bi, before[bi].clone(), &mut after, &mut plan);
                bi += 1;
            }
            for l in &h.lines {
                let mut chars = l.chars();
                let mark = chars.next().unwrap_or(' ');
                let text = chars.as_str();
                match mark {
                    '+' => {
                        after.push(text.to_string());
                        plan.push((RowKind::Add, None, Some(after.len()), Src::After(after.len() - 1)));
                    }
                    '-' => {
                        plan.push((RowKind::Del, Some(bi + 1), None, Src::Before(bi)));
                        bi += 1;
                    }
                    '\\' => {}
                    _ => {
                        let t = before.get(bi).cloned().unwrap_or_else(|| text.to_string());
                        push_ctx(bi, t, &mut after, &mut plan);
                        bi += 1;
                    }
                }
            }
        }
        while bi < before.len() {
            push_ctx(bi, before[bi].clone(), &mut after, &mut plan);
            bi += 1;
        }
    }

    let hb = hl.highlight(&e.path, &before);
    let ha = hl.highlight(&e.path, &after);
    plan.into_iter()
        .map(|(kind, old_no, new_no, src)| Row {
            kind,
            old_no,
            new_no,
            spans: match src {
                Src::Before(i) => hb.get(i).cloned().unwrap_or_default(),
                Src::After(i) => ha.get(i).cloned().unwrap_or_default(),
            },
        })
        .collect()
}

/// 수정 전 파일이 기록에 없을 때: diff 조각마다 따로 칠한다(조각 첫 줄에서 문법 상태가 끊길 수 있다).
fn fragment_rows(e: &EditEntry, hl: &Highlighter) -> Vec<Row> {
    let mut rows = Vec::new();
    for (n, h) in e.hunks.iter().enumerate() {
        if n > 0 {
            rows.push(Row { kind: RowKind::Sep, old_no: None, new_no: None, spans: Vec::new() });
        }
        let texts: Vec<String> = h.lines.iter().map(|l| l.chars().skip(1).collect()).collect();
        let colored = hl.highlight(&e.path, &texts);
        let (mut old, mut new) = (h.old_start, h.new_start);
        for (l, spans) in h.lines.iter().zip(colored) {
            let (kind, o, nn) = match l.chars().next() {
                Some('+') => {
                    new += 1;
                    (RowKind::Add, None, Some(new - 1))
                }
                Some('-') => {
                    old += 1;
                    (RowKind::Del, Some(old - 1), None)
                }
                Some('\\') => continue,
                _ => {
                    old += 1;
                    new += 1;
                    (RowKind::Ctx, Some(old - 1), Some(new - 1))
                }
            };
            rows.push(Row { kind, old_no: o, new_no: nn, spans });
        }
    }
    rows
}

/// 바뀐 줄 앞뒤 CONTEXT 줄만 남긴다. None 은 생략 표시 자리.
pub fn compact(rows: &[Row]) -> Vec<Option<usize>> {
    let changes: Vec<usize> = rows.iter().enumerate().filter(|(_, r)| r.is_change()).map(|(i, _)| i).collect();
    if changes.is_empty() {
        return (0..rows.len().min(CONTEXT * 4)).map(Some).collect();
    }
    let mut keep = vec![false; rows.len()];
    for &c in &changes {
        for k in keep.iter_mut().take((c + CONTEXT + 1).min(rows.len())).skip(c.saturating_sub(CONTEXT)) {
            *k = true;
        }
    }
    let mut out = Vec::new();
    let mut gap = false;
    for (i, k) in keep.iter().enumerate() {
        if *k || rows[i].kind == RowKind::Sep {
            if gap && !out.is_empty() {
                out.push(None);
            }
            gap = false;
            out.push(Some(i));
        } else {
            gap = true;
        }
    }
    out
}
