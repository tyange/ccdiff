//! tmux pane 과 Claude 세션 기록을 짝짓는다.
//! pane 셸 아래 프로세스 중 `sessions/<pid>.json` 이 있는 것이 Claude 다. 그 파일의 sessionId 로 기록을 찾고,
//! 파일의 `tmux` 필드가 pane ID 와 같은지 대조한다. 파일이 없으면 cwd 의 최근 기록으로 추정한다.

use std::{collections::HashSet, path::PathBuf};

use crate::{
    claude::{self, SessionFile},
    procs::Procs,
    tmux::Pane,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// 세션 파일이 있고 `tmux` 필드가 이 pane 이다.
    Confirmed,
    /// 세션 파일은 있지만 `tmux` 필드가 없다(tmux 밖에서 띄운 뒤 붙었거나 옛 버전).
    NoTmuxField,
    /// 세션 파일의 `tmux` 필드가 다른 pane 을 가리킨다 — pane 을 옮겼거나 파일이 낡았다.
    TmuxMismatch,
    /// 세션 파일이 없어 cwd 의 최근 기록으로 추정했다.
    Guess,
}

impl How {
    pub fn as_str(self) -> &'static str {
        match self {
            How::Confirmed => "confirmed",
            How::NoTmuxField => "no-tmux-field",
            How::TmuxMismatch => "tmux-mismatch",
            How::Guess => "guess",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Found {
    pub pane: Pane,
    pub pid: u32,
    pub file: Option<SessionFile>,
    pub jsonl: Option<PathBuf>,
    pub how: How,
    /// 이 세션이 넘긴 백그라운드 작업(tmux 밖 데몬 PTY 에서 돈다).
    pub parked: Option<SessionFile>,
}

impl Found {
    /// 목록 키: 세션 ID, 추정이면 pane ID.
    pub fn key(&self) -> String {
        match &self.file {
            Some(f) => f.session_id.clone(),
            None => format!("pane:{}", self.pane.id),
        }
    }
}

pub fn find(panes: &[Pane], procs: &Procs) -> Vec<Found> {
    let mut all: Option<Vec<SessionFile>> = None;
    let mut guesses = Vec::new();
    let mut out = Vec::new();
    for pane in panes {
        let pids = procs.descendants(pane.pid);
        let with_file = pids
            .iter()
            .find_map(|&p| claude::session_file(p).filter(|f| procs.same_process(p, f.proc_start.as_deref())).map(|f| (p, f)));
        let found = match with_file {
            Some((pid, file)) => {
                let how = match file.tmux_pane() {
                    Some(p) if p == pane.id => How::Confirmed,
                    Some(_) => How::TmuxMismatch,
                    None => How::NoTmuxField,
                };
                let jsonl = claude::find_jsonl(&file.session_id, &file.cwd);
                let parked = file.parked_job.as_ref().and_then(|job| {
                    all.get_or_insert_with(claude::all_session_files)
                        .iter()
                        .find(|f| f.job_id.as_ref() == Some(job) && procs.same_process(f.pid, f.proc_start.as_deref()))
                        .cloned()
                });
                Found { pane: pane.clone(), pid, file: Some(file), jsonl, how, parked }
            }
            None => {
                let Some(&pid) = pids.iter().find(|&&p| procs.command(p).is_some_and(claude::looks_like_claude)) else {
                    continue;
                };
                // 기록은 다 훑은 뒤에 고른다 — 다른 세션이 차지한 기록을 빼야 해서.
                guesses.push(out.len());
                Found { pane: pane.clone(), pid, file: None, jsonl: None, how: How::Guess, parked: None }
            }
        };
        out.push(found);
    }
    if !guesses.is_empty() {
        let claimed: HashSet<String> = all
            .get_or_insert_with(claude::all_session_files)
            .iter()
            .filter(|f| procs.same_process(f.pid, f.proc_start.as_deref()))
            .map(|f| f.session_id.clone())
            .collect();
        for i in guesses {
            out[i].jsonl = claude::newest_jsonl(&out[i].pane.path, &claimed);
        }
    }
    out
}
