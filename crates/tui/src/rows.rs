//! core 가 펼친 줄 목록에 색을 입힌다. 화면·재생이 모두 이 목록을 쓴다.

use ratatui::style::Color;

pub use ccdiff_core::rows::{RowKind, Sources};
use ccdiff_core::{rows as core_rows, session::EditEntry};

use crate::highlight::{Highlighter, Spans};

pub struct Row {
    pub kind: RowKind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub spans: Spans,
    /// 색을 가져올 줄: (수정 전 파일인가, 줄 인덱스). 하이라이트가 끝나면 여기서 색을 채운다.
    pub src: Option<(bool, usize)>,
}

fn plain(text: String) -> Spans {
    if text.is_empty() { Vec::new() } else { vec![(Color::Reset, text)] }
}

/// 백그라운드에서 칠한 결과를 줄들에 입힌다.
pub fn apply_highlight(rows: &mut [Row], before: &[Spans], after: &[Spans]) {
    for r in rows {
        let colored = match r.src {
            Some((true, i)) => before.get(i),
            Some((false, i)) => after.get(i),
            None => None,
        };
        if let Some(c) = colored {
            r.spans = c.clone();
        }
    }
}

impl Row {
    pub fn width(&self) -> usize {
        self.spans.iter().map(|(_, t)| t.chars().count()).sum()
    }

    pub fn is_change(&self) -> bool {
        matches!(self.kind, RowKind::Add | RowKind::Del)
    }
}

/// 줄 목록은 바로 만들고(색 없음), 칠할 원문은 따로 돌려준다 — 하이라이트는 느려서 백그라운드로 보낸다.
/// 원문이 None 이면 이미 칠해진 것이다(diff 조각만 있는 경우).
pub fn build_rows(e: &EditEntry, hl: &Highlighter) -> (Vec<Row>, Option<Sources>) {
    let (plain_rows, src) = core_rows::build_rows(e);
    let mut colored = if src.is_none() { fragment_colors(e, hl).into_iter() } else { Vec::new().into_iter() };
    let rows = plain_rows
        .into_iter()
        .map(|r| {
            let spans = match (r.src, r.kind) {
                (None, RowKind::Sep) => Vec::new(),
                (None, _) => colored.next().unwrap_or_else(|| plain(r.text)),
                _ => plain(r.text),
            };
            Row { kind: r.kind, old_no: r.old_no, new_no: r.new_no, spans, src: r.src }
        })
        .collect();
    (rows, src)
}

/// 수정 전 파일이 기록에 없을 때: diff 조각마다 따로 칠한다(조각 첫 줄에서 문법 상태가 끊길 수 있다).
/// core 의 조각 줄과 같은 순서(`\` 줄 제외)로 돌려준다.
fn fragment_colors(e: &EditEntry, hl: &Highlighter) -> Vec<Spans> {
    let mut out = Vec::new();
    for h in &e.hunks {
        let texts: Vec<String> = h.lines.iter().map(|l| l.chars().skip(1).collect()).collect();
        let colored = hl.highlight(&e.path, &texts);
        for (l, spans) in h.lines.iter().zip(colored) {
            if !l.starts_with('\\') {
                out.push(spans);
            }
        }
    }
    out
}

/// 바뀐 줄 앞뒤 CONTEXT 줄만 남긴다. None 은 생략 표시 자리.
pub fn compact(rows: &[Row]) -> Vec<Option<usize>> {
    core_rows::compact_by(rows.len(), |i| rows[i].kind)
}
