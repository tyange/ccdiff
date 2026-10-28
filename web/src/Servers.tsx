// 서버 칸: dev 서버를 워크트리·포트로 짝지어 보이고, pane 서버는 로그(시크릿은 서버가 가림)와 확인을 거친 재시작.
// pane 밖 서버(Claude 의 Bash 가 띄운 것)는 출력이 버려져 로그가 없고, 남의 것이라 재시작하지 않는다.

import { useEffect, useRef, useState } from "react";

import { getJson, postJson, wsArgs } from "./api";
import { Modal } from "./Dialogs";
import type { DevServer, Session } from "./types";

interface Plan {
  pane: string;
  command: string;
  warnings: string[];
  nonce: string;
}

function RestartDialog(props: { server: DevServer; onClose: () => void; onDone: (msg: string) => void }) {
  const { server, onClose, onDone } = props;
  const [plan, setPlan] = useState<Plan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    postJson<Plan>("/api/servers/restart/plan", { pane: server.id }).then(setPlan, (e: Error) => setError(e.message));
  }, [server.id]);
  const run = async () => {
    if (!plan) return;
    setBusy(true);
    try {
      await postJson("/api/servers/restart", { nonce: plan.nonce });
      onDone(`${server.name} 을 다시 띄웠다`);
    } catch (e) {
      setError((e as Error).message);
      setBusy(false);
    }
  };
  return (
    <Modal title={`${server.name} 다시 시작`} onClose={onClose}>
      {!plan && !error && <p>계획을 읽는 중…</p>}
      {plan && (
        <ul className="warnings">
          {plan.warnings.map((w) => (
            <li key={w}>{w}</li>
          ))}
        </ul>
      )}
      {busy && <p className="muted">멈추기를 기다리는 중(최대 15초)…</p>}
      {error && <p className="error">{error}</p>}
      <div className="actions">
        <button autoFocus onClick={onClose}>
          취소
        </button>
        <button className="primary" disabled={!plan || busy} onClick={run}>
          다시 시작
        </button>
      </div>
    </Modal>
  );
}

function Log({ pane }: { pane: string }) {
  const [text, setText] = useState("");
  const [state, setState] = useState<"connecting" | "open" | "gone">("connecting");
  const box = useRef<HTMLPreElement>(null);
  const stick = useRef(true);
  useEffect(() => {
    setText("");
    setState("connecting");
    const sock = new WebSocket(...wsArgs(`/ws/log/${encodeURIComponent(pane)}`));
    sock.onmessage = (m) => {
      const msg = JSON.parse(m.data);
      if (msg.type === "log") {
        setText(msg.text);
        setState("open");
      }
      if (msg.type === "gone") setState("gone");
    };
    return () => sock.close();
  }, [pane]);
  // 맨 아래를 보고 있었을 때만 따라 내려간다.
  useEffect(() => {
    const b = box.current;
    if (b && stick.current) b.scrollTop = b.scrollHeight;
  }, [text]);
  return (
    <pre
      className="log"
      ref={box}
      onScroll={(e) => {
        const b = e.currentTarget;
        stick.current = b.scrollHeight - b.scrollTop - b.clientHeight < 24;
      }}
    >
      {state === "gone" ? "pane 이 사라졌다" : text || "로그를 읽는 중…"}
    </pre>
  );
}

export function Servers({ session }: { session: Session | null }) {
  const [servers, setServers] = useState<DevServer[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [restarting, setRestarting] = useState<DevServer | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    const load = () =>
      getJson<DevServer[]>("/api/servers").then(
        (x) => live && (setServers(x), setError(null)),
        (e: Error) => live && setError(e.message),
      );
    load();
    const t = window.setInterval(load, 5000);
    return () => {
      live = false;
      window.clearInterval(t);
    };
  }, []);

  useEffect(() => {
    if (!notice) return;
    const t = window.setTimeout(() => setNotice(null), 5000);
    return () => window.clearTimeout(t);
  }, [notice]);

  // 지금 고른 세션의 워크트리 서버가 위로.
  const mine = (s: DevServer) => !!s.worktree && s.worktree.root === session?.worktree?.root;
  const rows = [...servers].sort((a, b) => Number(mine(b)) - Number(mine(a)));
  const current = servers.find((s) => s.id === selected) ?? null;

  return (
    <div className="servers">
      <div className="srv-list">
        {rows.length === 0 && <p className="empty">{error ?? "dev 서버를 찾지 못했다"}</p>}
        {rows.map((s) => (
          <div
            key={s.id}
            className={`srv${s.id === selected ? " selected" : ""}${mine(s) ? " mine" : ""}`}
            title={s.command}
            onMouseDown={(e) => {
              e.preventDefault();
              setSelected(s.id);
            }}
          >
            <span className={`dot ${s.running ? "busy" : "stopped"}`} />
            <span className="srv-name">{s.name}</span>
            <span className="srv-wt">{s.worktree?.issue ? `gh${s.worktree.issue}` : (s.worktree?.name ?? "")}</span>
            <span className="srv-ports">
              {s.ports.map((p) => (
                <a key={p} href={`http://localhost:${p}/`} target="_blank" rel="noreferrer" onMouseDown={(e) => e.stopPropagation()}>
                  :{p}
                </a>
              ))}
            </span>
            <span className="srv-owner">{s.kind === "detached" ? `pane 밖 · ${s.owner ?? "띄운 쪽 모름"}` : s.running ? "" : "멈춤"}</span>
          </div>
        ))}
      </div>
      <div className="srv-detail">
        {current ? (
          <>
            <div className="srv-head">
              <strong>{current.name}</strong>
              <code className="srv-cmd">{current.command || "(명령 모름)"}</code>
              <span className="spacer" />
              {current.kind === "pane" && (
                <button disabled={!current.restartable} onClick={() => setRestarting(current)}>
                  다시 시작…
                </button>
              )}
            </div>
            {current.kind === "pane" ? (
              <Log pane={current.id} />
            ) : (
              <p className="empty">
                pane 밖에서 띄운 서버다(띄운 쪽: {current.owner ?? "모름"}). 출력이 버려져 로그가 없고, 남의 것이라 여기서 재시작하지 않는다.
              </p>
            )}
          </>
        ) : (
          <p className="empty">서버를 고르면 로그가 보인다 · 로그의 키·토큰처럼 보이는 값은 가린다</p>
        )}
        {notice && <div className="notice">{notice}</div>}
      </div>
      {restarting && (
        <RestartDialog
          server={restarting}
          onClose={() => setRestarting(null)}
          onDone={(msg) => {
            setRestarting(null);
            setNotice(msg);
          }}
        />
      )}
    </div>
  );
}
