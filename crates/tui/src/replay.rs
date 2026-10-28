//! 한 파일의 수정들을 순서대로 재생한다: 지울 줄 표시 → 새 줄 타이핑 → 잠깐 멈춤 → 다음 수정.

use std::time::{Duration, Instant};

const DELETE_SHOW: Duration = Duration::from_millis(700);
const HOLD: Duration = Duration::from_millis(900);
const TYPE_MIN: f64 = 0.6;
const TYPE_MAX: f64 = 3.0;
const CHARS_PER_SEC: f64 = 90.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Delete,
    Type,
    Hold,
}

pub struct Replay {
    pub phase: Phase,
    pub typed: usize,
    pub paused: bool,
    elapsed: Duration,
    last: Instant,
}

pub enum Step {
    Stay,
    /// 이 수정의 재생이 끝났다 — 다음 수정으로 넘어가야 한다.
    Next,
}

impl Replay {
    pub fn new() -> Self {
        Self { phase: Phase::Delete, typed: 0, paused: false, elapsed: Duration::ZERO, last: Instant::now() }
    }

    pub fn restart_edit(&mut self) {
        self.phase = Phase::Delete;
        self.typed = 0;
        self.elapsed = Duration::ZERO;
    }

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.last = Instant::now();
    }

    /// total = 이 수정에서 타이핑할 글자 수(줄바꿈 포함), has_del = 지우는 줄이 있는가.
    pub fn tick(&mut self, total: usize, has_del: bool) -> Step {
        let now = Instant::now();
        if !self.paused {
            self.elapsed += now - self.last;
        }
        self.last = now;
        match self.phase {
            Phase::Delete => {
                if !has_del || self.elapsed >= DELETE_SHOW {
                    self.phase = Phase::Type;
                    self.elapsed = Duration::ZERO;
                }
            }
            Phase::Type => {
                let secs = (total as f64 / CHARS_PER_SEC).clamp(TYPE_MIN, TYPE_MAX);
                let ratio = self.elapsed.as_secs_f64() / secs;
                self.typed = ((total as f64) * ratio).min(total as f64) as usize;
                if self.typed >= total {
                    self.phase = Phase::Hold;
                    self.elapsed = Duration::ZERO;
                }
            }
            Phase::Hold => {
                if self.elapsed >= HOLD {
                    return Step::Next;
                }
            }
        }
        Step::Stay
    }
}
