//! ccdiff 와 앱 서버가 함께 쓰는 코어: 세션 기록 읽기, 수정 펼치기, 커밋 판정, 작업 트리, IDE 연결.

pub mod git;
pub mod ide;
pub mod rows;
pub mod session;
pub mod worktree;
