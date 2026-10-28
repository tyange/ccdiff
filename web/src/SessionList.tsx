import type { Session } from "./types";

/** 방금 · N분 전 · N시간 전 · MM-DD */
function ago(epoch: number, now: number): string {
  if (!epoch) return "기록 없음";
  const s = Math.max(0, now - epoch);
  if (s < 60) return "방금";
  if (s < 3600) return `${Math.floor(s / 60)}분 전`;
  if (s < 86400) return `${Math.floor(s / 3600)}시간 전`;
  const d = new Date(epoch * 1000);
  return `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

const MATCH_NOTE: Record<string, string | null> = {
  confirmed: null,
  "no-tmux-field": "세션 파일에 tmux 정보 없음",
  "tmux-mismatch": "세션 파일이 다른 pane 을 가리킴",
  guess: "세션 파일 없음 — 최근 기록으로 추정",
};

function title(s: Session): string {
  const w = s.worktree;
  if (w?.issue) return `gh${w.issue}`;
  return s.pane.agent || w?.name || s.pane.windowName;
}

export function SessionList(props: {
  sessions: Session[];
  selected: string | null;
  onSelect: (key: string) => void;
  now: number;
}) {
  const { sessions, selected, onSelect, now } = props;
  if (sessions.length === 0) {
    return <p className="empty">tmux 에서 도는 Claude 가 없다</p>;
  }
  return (
    <ul className="sessions" role="listbox" aria-label="세션">
      {sessions.map((s, i) => {
        const w = s.worktree;
        const note = MATCH_NOTE[s.match];
        return (
          <li
            key={s.key}
            role="option"
            aria-selected={s.key === selected}
            className={s.key === selected ? "session selected" : "session"}
            onMouseDown={(e) => {
              // 목록을 눌러도 키보드 포커스는 원래 칸(터미널)에 남긴다.
              e.preventDefault();
              onSelect(s.key);
            }}
          >
            <div className="row1">
              {i < 9 && <kbd>⌘{i + 1}</kbd>}
              <span className={`dot ${s.status ?? "unknown"}`} title={s.status ?? "상태 모름"} />
              <span className="title">{title(s)}</span>
              <span className="when">{ago(s.lastActivity, now)}</span>
            </div>
            <div className="row2">{w ? w.branch || w.name : s.cwd}</div>
            <div className="row3">
              <span>{s.pane.agent || s.pane.windowName}</span>
              {w && w.dirty > 0 && <span className="dirty">미커밋 {w.dirty}</span>}
              <span>수정 {s.edits}</span>
              {s.worktrees.length > 1 && <span>워크트리 {s.worktrees.length}</span>}
            </div>
            {s.parked && <div className="note">⧗ 백그라운드 작업으로 넘김 · {s.parked.status}</div>}
            {note && <div className="note warn">⚠ {note}</div>}
          </li>
        );
      })}
    </ul>
  );
}
