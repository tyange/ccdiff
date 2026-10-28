// 서버 접속. 토큰은 주소의 #t= 로 받아 탭 저장소로 옮기고 주소에서 지운다 — 방문 기록·화면 공유에 남지 않게.
// HTTP 는 Authorization 헤더, WebSocket 은 하위 프로토콜 auth.<토큰> 으로 싣는다(crates/server/src/guard.rs).

import type { ServerEvent } from "./types";

const KEY = "worktable-token";

function takeToken(): string | null {
  const m = location.hash.match(/(?:^#|&)t=([0-9a-f]+)/);
  try {
    if (m) {
      sessionStorage.setItem(KEY, m[1]);
      history.replaceState(null, "", location.pathname);
    }
    return sessionStorage.getItem(KEY) ?? m?.[1] ?? null;
  } catch {
    return m?.[1] ?? null;
  }
}

export const token = takeToken();

export async function getJson<T>(path: string): Promise<T> {
  const r = await fetch(path, { headers: { Authorization: `Bearer ${token}` } });
  if (!r.ok) throw new Error(`${path} ${r.status}`);
  return r.json() as Promise<T>;
}

export type Conn = "connecting" | "open" | "closed" | "no-token";

/** /ws/events 에 붙고, 끊기면 1초 → 최대 10초 간격으로 다시 붙는다. 돌려준 함수로 끊는다. */
export function subscribe(onEvent: (e: ServerEvent) => void, onConn: (c: Conn) => void): () => void {
  if (!token) {
    onConn("no-token");
    return () => {};
  }
  let ws: WebSocket | null = null;
  let stopped = false;
  let delay = 1000;
  let timer: number | undefined;
  const open = () => {
    onConn("connecting");
    ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws/events`, [
      "worktable.v1",
      `auth.${token}`,
    ]);
    ws.onopen = () => {
      delay = 1000;
      onConn("open");
    };
    ws.onmessage = (m) => onEvent(JSON.parse(m.data) as ServerEvent);
    ws.onclose = () => {
      onConn("closed");
      if (!stopped) {
        timer = window.setTimeout(open, delay);
        delay = Math.min(delay * 2, 10_000);
      }
    };
  };
  open();
  return () => {
    stopped = true;
    window.clearTimeout(timer);
    ws?.close();
  };
}
