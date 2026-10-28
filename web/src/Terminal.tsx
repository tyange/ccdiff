// 고른 세션의 tmux pane 에 붙은 터미널. 서버가 PTY 에서 `tmux attach` 를 돌리고 바이트를 그대로 주고받는다.

import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal as XTerm } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import { wsArgs } from "./api";
import type { Session } from "./types";

export interface TerminalHandle {
  focus(): void;
}

type Attach =
  | { state: "idle" }
  | { state: "connecting" }
  | { state: "attached"; owned: boolean; session: string }
  | { state: "closed"; reason: string };

const DARK = { background: "#0d1117", foreground: "#e6edf3", cursor: "#e6edf3", selectionBackground: "#264f78" };
const LIGHT = { background: "#ffffff", foreground: "#1f2328", cursor: "#1f2328", selectionBackground: "#b6d6fd" };

const prefersDark = () => window.matchMedia("(prefers-color-scheme: dark)").matches;

export const Terminal = forwardRef<TerminalHandle, { session: Session | null; onImport: (s: Session) => void }>(
  function Terminal({ session, onImport }, ref) {
    const box = useRef<HTMLDivElement>(null);
    const term = useRef<XTerm | null>(null);
    const fit = useRef<FitAddon | null>(null);
    const ws = useRef<WebSocket | null>(null);
    const [attach, setAttach] = useState<Attach>({ state: "idle" });
    const [retry, setRetry] = useState(0);

    useImperativeHandle(ref, () => ({ focus: () => term.current?.focus() }), []);

    // xterm 은 한 번만 만들고 세션이 바뀌면 화면만 비운다.
    useEffect(() => {
      const t = new XTerm({
        fontFamily: '"JetBrains Mono", "SF Mono", Menlo, monospace',
        fontSize: 13,
        cursorBlink: true,
        scrollback: 0, // 스크롤은 tmux 가 한다(복사 모드).
        theme: prefersDark() ? DARK : LIGHT,
      });
      const f = new FitAddon();
      t.loadAddon(f);
      t.open(box.current!);
      const enc = new TextEncoder();
      const send = (b: Uint8Array<ArrayBuffer>) => {
        if (ws.current?.readyState === WebSocket.OPEN) ws.current.send(b);
      };
      // ⌘ 키는 앱 단축키 — 터미널로 보내지 않는다. Shift+Enter 는 Claude 가 줄바꿈으로 받는 ESC CR(Option+Enter)로.
      t.attachCustomKeyEventHandler((e) => {
        if (e.metaKey) return false;
        if (e.key === "Enter" && e.shiftKey && !e.ctrlKey && !e.altKey) {
          if (e.type === "keydown") send(enc.encode("\x1b\r"));
          return false;
        }
        return true;
      });
      t.onData((d) => send(enc.encode(d)));
      t.onBinary((d) => send(Uint8Array.from(d, (c) => c.charCodeAt(0))));
      t.onResize(({ cols, rows }) => {
        if (ws.current?.readyState === WebSocket.OPEN) ws.current.send(JSON.stringify({ type: "resize", cols, rows }));
      });
      const ro = new ResizeObserver(() => {
        try {
          f.fit();
        } catch {
          // 칸이 숨겨져 크기가 0 이면 맞추지 않는다.
        }
      });
      ro.observe(box.current!);
      const mq = window.matchMedia("(prefers-color-scheme: dark)");
      const onScheme = () => (t.options.theme = mq.matches ? DARK : LIGHT);
      mq.addEventListener("change", onScheme);
      term.current = t;
      fit.current = f;
      return () => {
        mq.removeEventListener("change", onScheme);
        ro.disconnect();
        t.dispose();
      };
    }, []);

    const paneId = session?.pane.id ?? null;
    // 가져오기로 세션이 바뀌면(abc → worktable) 붙는 방식이 달라지므로 다시 붙는다.
    const paneSession = session?.pane.session ?? null;

    useEffect(() => {
      const t = term.current;
      if (!t || !paneId) {
        setAttach({ state: "idle" });
        return;
      }
      t.reset();
      try {
        fit.current?.fit();
      } catch {
        // 아래 기본 크기로 붙는다.
      }
      setAttach({ state: "connecting" });
      const [url, protocols] = wsArgs(`/ws/term/${encodeURIComponent(paneId)}?cols=${t.cols}&rows=${t.rows}`);
      const sock = new WebSocket(url, protocols);
      sock.binaryType = "arraybuffer";
      ws.current = sock;
      let closedByUs = false;
      sock.onmessage = (m) => {
        if (typeof m.data === "string") {
          const msg = JSON.parse(m.data);
          if (msg.type === "attached") setAttach({ state: "attached", owned: msg.owned, session: msg.session });
          if (msg.type === "error") setAttach({ state: "closed", reason: msg.message });
          return;
        }
        t.write(new Uint8Array(m.data as ArrayBuffer));
      };
      sock.onclose = () => {
        if (closedByUs) return;
        setAttach((a) => (a.state === "closed" ? a : { state: "closed", reason: "연결이 끊겼다" }));
      };
      return () => {
        closedByUs = true;
        sock.close();
        if (ws.current === sock) ws.current = null;
      };
    }, [paneId, paneSession, retry]);

    let head: React.ReactNode = null;
    if (attach.state === "connecting") head = "붙는 중…";
    if (attach.state === "closed")
      head = (
        <>
          {attach.reason} <button onClick={() => setRetry((n) => n + 1)}>다시 붙기</button>
        </>
      );
    if (attach.state === "attached")
      head = attach.owned ? (
        <span title="worktable 세션 — 이 칸 크기를 따른다">worktable</span>
      ) : (
        <>
          <span title={`${attach.session} 세션 — 크기는 그 세션에 붙은 터미널을 따른다`}>보기 모드</span>
          {session && (
            <button className="link" onClick={() => onImport(session)}>
              가져오기…
            </button>
          )}
        </>
      );

    return (
      <div className="term">
        <div className="term-head">{head}</div>
        <div className="term-box" ref={box} />
      </div>
    );
  },
);
