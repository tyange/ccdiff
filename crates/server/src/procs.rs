//! 프로세스 표 한 장(`ps -axo pid,ppid,lstart,command`). pane 의 셸 아래에서 도는 Claude 를 찾는 데 쓴다.

use std::{collections::HashMap, process::Command};

#[derive(Default)]
pub struct Procs {
    children: HashMap<u32, Vec<u32>>,
    commands: HashMap<u32, String>,
    /// 시작 시각(UTC, `Wed Oct 28 04:35:22 2026` 꼴, 공백 하나로 맞춤).
    starts: HashMap<u32, String>,
}

impl Procs {
    pub fn snapshot() -> Self {
        let mut p = Self::default();
        // Claude 세션 파일의 procStart 가 UTC 라 같은 꼴로 받는다.
        let Ok(out) = Command::new("ps").env("TZ", "UTC").args(["-axo", "pid=,ppid=,lstart=,command="]).output() else { return p };
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut it = line.split_whitespace();
            let (Some(pid), Some(ppid)) = (it.next().and_then(|x| x.parse().ok()), it.next().and_then(|x| x.parse().ok())) else {
                continue;
            };
            p.children.entry(ppid).or_default().push(pid);
            let start: Vec<&str> = it.by_ref().take(5).collect();
            p.starts.insert(pid, start.join(" "));
            p.commands.insert(pid, it.collect::<Vec<_>>().join(" "));
        }
        p
    }

    /// pid 가 살아 있고 시작 시각도 같은가 — pid 가 다른 프로세스에 다시 쓰였으면 false.
    /// 시작 시각을 모르면 살아 있는지만 본다.
    pub fn same_process(&self, pid: u32, start: Option<&str>) -> bool {
        let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        match (self.starts.get(&pid), start) {
            (Some(have), Some(want)) => norm(have) == norm(want),
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    pub fn command(&self, pid: u32) -> Option<&str> {
        self.commands.get(&pid).map(String::as_str)
    }

    /// root 와 그 아래 프로세스들, 얕은 것부터.
    pub fn descendants(&self, root: u32) -> Vec<u32> {
        let mut out = vec![root];
        let mut i = 0;
        while i < out.len() {
            if let Some(cs) = self.children.get(&out[i]) {
                out.extend(cs);
            }
            i += 1;
        }
        out
    }
}
