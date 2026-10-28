//! 수정 하나를 "파일 전체 기준 줄 목록"으로 펼친다. 화면·재생·웹이 모두 이 목록을 쓴다.
//! 색은 여기서 칠하지 않는다 — 원문(`Sources`)을 돌려주면 쓰는 쪽이 칠한다.

use crate::session::EditEntry;

const MAX_LINES: usize = 20_000;
pub const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Ctx,
    Add,
    Del,
    /// 생략 구간 표시(diff 조각만 있을 때 조각 사이).
    Sep,
}

#[derive(Clone, Debug)]
pub struct PlainRow {
    pub kind: RowKind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub text: String,
    /// 색을 가져올 줄: (수정 전 파일인가, 줄 인덱스). None 이면 diff 조각만 있는 경우다.
    pub src: Option<(bool, usize)>,
}

impl PlainRow {
    pub fn is_change(&self) -> bool {
        matches!(self.kind, RowKind::Add | RowKind::Del)
    }
}

/// 하이라이트할 원문: (수정 전 줄들, 수정 후 줄들).
pub type Sources = (Vec<String>, Vec<String>);

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

/// 줄 목록과 원문을 돌려준다. 원문이 None 이면 수정 전 파일이 기록에 없어 diff 조각만으로 만든 것이다.
pub fn build_rows(e: &EditEntry) -> (Vec<PlainRow>, Option<Sources>) {
    let before: Vec<String> = match (&e.before, e.kind) {
        (Some(b), _) => b.lines().take(MAX_LINES).map(String::from).collect(),
        (None, "Write+") => Vec::new(),
        (None, _) => return (fragment_rows(e), None),
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

    let rows = plan
        .into_iter()
        .map(|(kind, old_no, new_no, src)| {
            let (text, src) = match src {
                Src::Before(i) => (before.get(i).cloned().unwrap_or_default(), (true, i)),
                Src::After(i) => (after.get(i).cloned().unwrap_or_default(), (false, i)),
            };
            PlainRow { kind, old_no, new_no, text, src: Some(src) }
        })
        .collect();
    (rows, Some((before, after)))
}

/// 수정 전 파일이 기록에 없을 때: diff 조각을 그대로 줄로 옮긴다. 조각 사이에는 Sep 을 넣는다.
fn fragment_rows(e: &EditEntry) -> Vec<PlainRow> {
    let mut rows = Vec::new();
    for (n, h) in e.hunks.iter().enumerate() {
        if n > 0 {
            rows.push(PlainRow { kind: RowKind::Sep, old_no: None, new_no: None, text: String::new(), src: None });
        }
        let (mut old, mut new) = (h.old_start, h.new_start);
        for l in &h.lines {
            let text: String = l.chars().skip(1).collect();
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
            rows.push(PlainRow { kind, old_no: o, new_no: nn, text, src: None });
        }
    }
    rows
}

/// 바뀐 줄 앞뒤 CONTEXT 줄만 남긴다. None 은 생략 표시 자리.
pub fn compact(rows: &[PlainRow]) -> Vec<Option<usize>> {
    compact_by(rows.len(), |i| rows[i].kind)
}

/// 줄 종류만 보고 접는다 — 줄 타입이 다른 쪽(TUI 의 색 있는 줄)도 같은 규칙을 쓴다.
pub fn compact_by(len: usize, kind: impl Fn(usize) -> RowKind) -> Vec<Option<usize>> {
    let changes: Vec<usize> = (0..len).filter(|&i| matches!(kind(i), RowKind::Add | RowKind::Del)).collect();
    if changes.is_empty() {
        return (0..len.min(CONTEXT * 4)).map(Some).collect();
    }
    let mut keep = vec![false; len];
    for &c in &changes {
        for k in keep.iter_mut().take((c + CONTEXT + 1).min(len)).skip(c.saturating_sub(CONTEXT)) {
            *k = true;
        }
    }
    let mut out = Vec::new();
    let mut gap = false;
    for (i, k) in keep.iter().enumerate() {
        if *k || kind(i) == RowKind::Sep {
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
