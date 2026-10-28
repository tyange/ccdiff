import { useEffect, useRef, useState } from "react";

import { subscribe, type Conn } from "./api";
import { Changes, type ChangesHandle } from "./Changes";
import { ImportDialog, NewSessionDialog } from "./Dialogs";
import { Palette, type Command } from "./Palette";
import { Servers } from "./Servers";
import { SessionList } from "./SessionList";
import { Terminal, type TerminalHandle } from "./Terminal";
import type { Session } from "./types";

type Panel = "sessions" | "terminal" | "changes" | "servers";

const PANELS: Panel[] = ["sessions", "terminal", "changes", "servers"];

const CONN_LABEL: Record<Conn, string> = {
  connecting: "연결 중",
  open: "연결됨",
  closed: "끊김 — 다시 붙는 중",
  "no-token": "토큰 없음 — worktable 이 출력한 주소로 여세요",
};

const SELECTED_KEY = "worktable-selected";

function loadSelected(): string | null {
  try {
    return sessionStorage.getItem(SELECTED_KEY);
  } catch {
    return null;
  }
}

export function App() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [conn, setConn] = useState<Conn>("connecting");
  const [selected, setSelected] = useState<string | null>(loadSelected);
  const [focused, setFocused] = useState<Panel | null>(null);
  const [now, setNow] = useState(() => Date.now() / 1000);
  const panels = useRef<Record<Panel, HTMLElement | null>>({ sessions: null, terminal: null, changes: null, servers: null });
  const terminal = useRef<TerminalHandle>(null);
  const changes = useRef<ChangesHandle>(null);
  const [importing, setImporting] = useState<Session | null>(null);
  const [starting, setStarting] = useState(false);
  const [palette, setPalette] = useState(false);
  // 새로 띄운 pane — 목록에 나타나면 고른다(Claude 가 세션 파일을 쓰기까지 몇 초 걸린다).
  const [pendingPane, setPendingPane] = useState<string | null>(null);

  useEffect(
    () =>
      subscribe((e) => {
        if (e.type === "sessions") setSessions(e.sessions);
      }, setConn),
    [],
  );

  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now() / 1000), 30_000);
    return () => window.clearInterval(t);
  }, []);

  // 고른 세션이 목록에서 사라지면 맨 위 세션으로.
  const current = sessions.find((s) => s.key === selected) ?? sessions[0] ?? null;

  const select = (key: string) => {
    setSelected(key);
    try {
      sessionStorage.setItem(SELECTED_KEY, key);
    } catch {
      // 저장 못 해도 고르기는 된다.
    }
    // 고르고 바로 입력할 수 있게 터미널로.
    terminal.current?.focus();
  };

  useEffect(() => {
    if (!pendingPane) return;
    const s = sessions.find((x) => x.pane.id === pendingPane);
    if (s) {
      setPendingPane(null);
      select(s.key);
    }
  }, [sessions, pendingPane]);

  // 앱 단축키는 ⌘ 계열만. 캡처 단계에서 먼저 받아 터미널로 넘기지 않는다.
  const latest = useRef({ sessions, focused, select });
  latest.current = { sessions, focused, select };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.metaKey || e.ctrlKey || e.altKey) return;
      const { sessions, focused, select } = latest.current;
      let handled = true;
      if (/^Digit[1-9]$/.test(e.code)) {
        const s = sessions[Number(e.code.slice(5)) - 1];
        if (s) select(s.key);
      } else if (e.code === "KeyJ") {
        // 변경 칸으로 갔다가, 다시 누르면 터미널로 돌아온다.
        if (focused === "changes") terminal.current?.focus();
        else panels.current.changes?.focus();
      } else if (e.code === "KeyK") {
        setPalette((open) => !open);
      } else if (e.code === "KeyE") {
        changes.current?.openInEditor();
      } else if (e.code === "ArrowUp" || e.code === "ArrowDown") {
        // 포커스는 그대로 두고 변경 칸만 움직인다 — 터미널에 입력하던 중이어도 된다.
        const d = e.code === "ArrowUp" ? -1 : 1;
        if (e.shiftKey) changes.current?.stepEdit(d);
        else changes.current?.stepFile(d);
      } else {
        handled = false;
      }
      if (handled) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, []);

  // 지금 키 입력이 가는 칸을 테두리로 보인다.
  useEffect(() => {
    const onFocus = () => {
      const a = document.activeElement;
      setFocused(PANELS.find((p) => panels.current[p]?.contains(a)) ?? null);
    };
    // 포커스가 칸 밖으로 빠질 때는 focusout 직후 activeElement 가 바뀌므로 한 박자 뒤에 본다.
    const onBlur = () => window.setTimeout(onFocus, 0);
    document.addEventListener("focusin", onFocus);
    document.addEventListener("focusout", onBlur);
    return () => {
      document.removeEventListener("focusin", onFocus);
      document.removeEventListener("focusout", onBlur);
    };
  }, []);

  const panel = (p: Panel, label: string, body: React.ReactNode) => (
    <section
      ref={(el) => {
        panels.current[p] = el;
      }}
      tabIndex={-1}
      // 칸 클래스는 p- 를 붙인다 — 안쪽 컴포넌트 클래스(changes 등)와 겹치지 않게.
      className={`panel p-${p}${focused === p ? " focused" : ""}`}
      aria-label={label}
    >
      <header className="panel-head">{label}</header>
      <div className="panel-body">{body}</div>
    </section>
  );

  const sessionLabel = (s: Session) => (s.worktree?.issue ? `gh${s.worktree.issue}` : s.pane.agent || s.worktree?.name || s.pane.windowName);
  const commands: Command[] = [
    ...sessions.map((s, i) => ({
      id: `s:${s.key}`,
      label: `세션 ${sessionLabel(s)}`,
      detail: [s.worktree?.branch, s.pane.windowName, s.pane.agent].filter(Boolean).join(" · "),
      keys: i < 9 ? `⌘${i + 1}` : undefined,
      run: () => select(s.key),
    })),
    { id: "new", label: "새 세션 시작…", run: () => setStarting(true) },
    ...(current && current.pane.session !== "worktable"
      ? [{ id: "import", label: `worktable 로 가져오기… (${sessionLabel(current)})`, run: () => setImporting(current) }]
      : []),
    { id: "terminal", label: "터미널로", run: () => terminal.current?.focus() },
    { id: "changes", label: "변경 칸으로", keys: "⌘J", run: () => panels.current.changes?.focus() },
    { id: "servers", label: "서버 칸으로", run: () => panels.current.servers?.focus() },
    { id: "editor", label: "편집기에서 열기", keys: "⌘E", run: () => changes.current?.openInEditor() },
  ];

  const w = current?.worktree;
  return (
    <div className="app">
      <div className="topbar">
        <span className={`conn ${conn}`}>{CONN_LABEL[conn]}</span>
        {current && (
          <span className="crumbs">
            {current.pane.session}:{current.pane.windowIndex} {current.pane.windowName}
            {w && ` · ${w.name} · ${w.branch}`}
          </span>
        )}
        {pendingPane && <span>새 세션 기다리는 중…</span>}
        <button onClick={() => setStarting(true)}>새 세션…</button>
        <span className="hint">⌘K 명령 · ⌘1–9 세션 · ⌘J 변경 칸 · ⌘↑↓ 파일 · ⌘⇧↑↓ 수정 · ⌘E 편집기</span>
      </div>
      {panel(
        "sessions",
        `세션 ${sessions.length}`,
        <SessionList sessions={sessions} selected={current?.key ?? null} onSelect={select} now={now} />,
      )}
      {panel("terminal", "터미널", <Terminal ref={terminal} session={current} onImport={setImporting} />)}
      {panel(
        "changes",
        "변경",
        <Changes ref={changes} session={current} focusTerminal={() => terminal.current?.focus()} />,
      )}
      {panel("servers", "서버", <Servers session={current} />)}
      {importing && (
        <ImportDialog
          session={importing}
          onClose={() => setImporting(null)}
          onDone={() => {
            setImporting(null);
            terminal.current?.focus();
          }}
        />
      )}
      {palette && <Palette commands={commands} onClose={() => setPalette(false)} />}
      {starting && (
        <NewSessionDialog
          sessions={sessions}
          onClose={() => setStarting(false)}
          onStarted={(pane) => {
            setStarting(false);
            setPendingPane(pane);
          }}
        />
      )}
    </div>
  );
}
