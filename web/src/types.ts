// 서버 /api/sessions·/ws/events 가 보내는 세션 목록 형식(crates/server/src/registry.rs 의 snapshot).

export type Match = "confirmed" | "no-tmux-field" | "tmux-mismatch" | "guess";

export interface Worktree {
  root: string;
  name: string | null;
  branch: string;
  issue: number | null;
  /** git status --porcelain 줄 수 */
  dirty: number;
}

export interface Pane {
  id: string;
  session: string;
  windowId: string;
  windowIndex: number;
  windowName: string;
  agent: string;
  agentStatus: string;
  width: number;
  height: number;
}

export interface Session {
  key: string;
  sessionId: string | null;
  name: string | null;
  /** idle · busy · shell 등(Claude 세션 파일) */
  status: string | null;
  version: string | null;
  pid: number;
  cwd: string;
  match: Match;
  jsonl: string | null;
  compacts: number;
  edits: number;
  /** 유닉스 초. 기록이 없으면 0 */
  lastActivity: number;
  pane: Pane;
  worktree: Worktree | null;
  worktrees: Worktree[];
  parked: { sessionId: string; name: string; status: string; pid: number } | null;
}

export type ServerEvent =
  | { type: "hello"; version: string }
  | { type: "sessions"; sessions: Session[] };
