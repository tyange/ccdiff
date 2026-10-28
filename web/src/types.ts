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
  /** 커밋되지 않은 세션 수정(●) 수 */
  uncommitted: number;
  /** 이 세션의 Claude 가 worktable IDE 에 붙어 있는가 */
  ide: boolean;
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

/** ✓ committed · ◐ changed(커밋 전 더 고침) · ● uncommitted · ✗ gone · ◆ worktree(작업 트리) */
export type Mark = "committed" | "changed" | "uncommitted" | "gone" | "worktree" | "unknown";

export interface ChangeItem {
  id: string;
  kind: string;
  path: string;
  rel: string;
  root: string | null;
  worktree: string | null;
  epoch: number;
  added: number;
  removed: number;
  mark: Mark;
  commit: { hash: string; subject: string } | null;
  userModified: boolean;
  secret: boolean;
}

export interface DiffInfo {
  id: string;
  path: string;
  rel: string;
  root: string | null;
  kind: string;
  /** 같은 파일을 고친 항목들(시간순, 작업 트리 항목이 있으면 맨 끝) */
  seq: string[];
}

export type DiffResult =
  | { secret: true; info: DiffInfo }
  | { secret: false; info: DiffInfo; original: string; modified: string; partial: boolean; firstChange: number | null };
