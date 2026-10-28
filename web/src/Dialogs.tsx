// 확인이 필요한 동작의 창: 가져오기(남의 tmux 배치를 바꾼다), 새 세션 시작.

import { useEffect, useState } from "react";

import { getJson, postJson } from "./api";
import type { Session } from "./types";

function Modal(props: { title: string; onClose: () => void; children: React.ReactNode }) {
  const { onClose } = props;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="modal-back" onMouseDown={onClose}>
      <div className="modal" role="dialog" aria-label={props.title} onMouseDown={(e) => e.stopPropagation()}>
        <h2>{props.title}</h2>
        {props.children}
      </div>
    </div>
  );
}

interface Plan {
  pane: string;
  from: string;
  window: string;
  warnings: string[];
  nonce: string;
}

export function ImportDialog(props: { session: Session; onClose: () => void; onDone: () => void }) {
  const { session, onClose, onDone } = props;
  const [plan, setPlan] = useState<Plan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    postJson<Plan>("/api/import/plan", { pane: session.pane.id }).then(setPlan, (e: Error) => setError(e.message));
  }, [session.pane.id]);

  const run = async () => {
    if (!plan) return;
    setBusy(true);
    try {
      await postJson("/api/import", { nonce: plan.nonce });
      onDone();
    } catch (e) {
      setError((e as Error).message);
      setBusy(false);
    }
  };

  return (
    <Modal title="worktable 로 가져오기" onClose={onClose}>
      {!plan && !error && <p>계획을 읽는 중…</p>}
      {plan && (
        <ul className="warnings">
          {plan.warnings.map((w) => (
            <li key={w}>{w}</li>
          ))}
        </ul>
      )}
      {error && <p className="error">{error}</p>}
      <div className="actions">
        <button autoFocus onClick={onClose}>
          취소
        </button>
        <button className="primary" disabled={!plan || busy} onClick={run}>
          가져오기
        </button>
      </div>
    </Modal>
  );
}

interface Worktree {
  path: string;
  name: string;
  branch: string;
  issue: number | null;
}

export function NewSessionDialog(props: {
  sessions: Session[];
  onClose: () => void;
  onStarted: (pane: string) => void;
}) {
  const { sessions, onClose, onStarted } = props;
  const [list, setList] = useState<Worktree[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    getJson<Worktree[]>("/api/worktrees").then(setList, (e: Error) => setError(e.message));
  }, []);

  // 그 워크트리를 대표 워크트리로 둔 세션 수.
  const running = (path: string) => sessions.filter((s) => s.worktree?.root === path).length;

  const start = async (w: Worktree) => {
    setBusy(true);
    try {
      const r = await postJson<{ pane: string }>("/api/sessions/new", { path: w.path });
      onStarted(r.pane);
    } catch (e) {
      setError((e as Error).message);
      setBusy(false);
    }
  };

  return (
    <Modal title="새 세션 시작" onClose={onClose}>
      <p className="muted">이미 있는 워크트리에서 worktable 세션의 새 window 로 claude 를 띄운다.</p>
      {!list && !error && <p>워크트리를 읽는 중…</p>}
      {list && list.length === 0 && <p>세션들이 다룬 레포에서 워크트리를 찾지 못했다.</p>}
      {list && (
        <ul className="worktrees">
          {list.map((w) => (
            <li key={w.path}>
              <button disabled={busy} onClick={() => start(w)}>
                <strong>{w.issue ? `gh${w.issue}` : w.name}</strong>
                <span>{w.branch || "(브랜치 없음)"}</span>
                {running(w.path) > 0 && <span className="muted">세션 {running(w.path)}개 진행 중</span>}
              </button>
            </li>
          ))}
        </ul>
      )}
      {error && <p className="error">{error}</p>}
      <div className="actions">
        <button autoFocus onClick={onClose}>
          닫기
        </button>
      </div>
    </Modal>
  );
}
