//! 화면 그리기를 막지 않도록 느린 일을 백그라운드 스레드에서 한다.
//! - 하이라이트: 파일 전체를 칠한다(1만 줄 파일은 수백 ms).
//! - git: 작업 트리 diff 와 커밋 판정(워크트리 여럿이면 수 초).

use std::{
    path::PathBuf,
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Condvar, Mutex,
    },
    thread,
};

use crate::{
    git::{CommitQuery, CommitState, GitIndex},
    highlight::{Highlighter, Spans},
    session::EditEntry,
    worktree,
};

pub struct HlJob {
    pub key: usize,
    /// 테마가 바뀌거나 작업 트리를 다시 읽으면 올라간다 — 옛 결과를 버리는 데 쓴다.
    pub gen: u64,
    pub path: PathBuf,
    pub before: Vec<String>,
    pub after: Vec<String>,
    pub sel: (bool, usize),
}

pub struct HlDone {
    pub key: usize,
    pub gen: u64,
    pub before: Vec<Spans>,
    pub after: Vec<Spans>,
}

type Queue = Arc<(Mutex<Vec<HlJob>>, Condvar)>;

pub struct HlWorker {
    queue: Queue,
    pub rx: Receiver<HlDone>,
}

impl HlWorker {
    pub fn spawn() -> Self {
        let queue: Queue = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let (tx, rx) = mpsc::channel();
        let q = Arc::clone(&queue);
        thread::spawn(move || {
            let hl = Highlighter::new();
            loop {
                let job = {
                    let (lock, cv) = &*q;
                    let mut jobs = lock.lock().expect("hl queue");
                    while jobs.is_empty() {
                        jobs = cv.wait(jobs).expect("hl queue");
                    }
                    // 가장 최근에 고른 항목부터 — 방향키로 훑고 지나간 항목은 뒤로 밀린다.
                    jobs.pop().expect("non-empty")
                };
                let before = hl.highlight_as(&job.path, &job.before, job.sel);
                let after = hl.highlight_as(&job.path, &job.after, job.sel);
                if tx.send(HlDone { key: job.key, gen: job.gen, before, after }).is_err() {
                    return;
                }
            }
        });
        Self { queue, rx }
    }

    pub fn push(&self, job: HlJob) {
        let (lock, cv) = &*self.queue;
        let mut jobs = lock.lock().expect("hl queue");
        jobs.retain(|j| j.key != job.key);
        jobs.push(job);
        cv.notify_one();
    }

    /// 옛 세대의 대기 작업을 버린다.
    pub fn drop_older(&self, gen: u64) {
        self.queue.0.lock().expect("hl queue").retain(|j| j.gen >= gen);
    }
}

pub struct GitReq {
    pub roots: Vec<PathBuf>,
    pub queries: Vec<CommitQuery>,
    pub since: i64,
}

pub struct GitDone {
    pub wt: Vec<EditEntry>,
    pub commits: Vec<(usize, CommitState)>,
}

pub struct GitWorker {
    tx: Sender<GitReq>,
    pub rx: Receiver<GitDone>,
}

/// 백그라운드와 --list·--verify 가 같은 계산을 쓴다.
pub fn run_git(index: &mut GitIndex, req: &GitReq) -> GitDone {
    index.expire();
    let wt = req.roots.iter().flat_map(|r| worktree::entries(r)).collect();
    let commits = req.queries.iter().map(|q| (q.key, index.state(q, req.since))).collect();
    GitDone { wt, commits }
}

impl GitWorker {
    pub fn spawn() -> Self {
        let (tx, req_rx) = mpsc::channel::<GitReq>();
        let (done_tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut index = GitIndex::default();
            while let Ok(req) = req_rx.recv() {
                if done_tx.send(run_git(&mut index, &req)).is_err() {
                    return;
                }
            }
        });
        Self { tx, rx }
    }

    pub fn send(&self, req: GitReq) {
        let _ = self.tx.send(req);
    }
}
