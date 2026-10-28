// ⌘K 명령 팔레트: 세션 전환과 앱 동작을 이름으로 찾아 Enter 로 실행한다. 닫으면 포커스는 원래 칸으로 돌아간다.

import { useEffect, useMemo, useRef, useState } from "react";

export interface Command {
  id: string;
  label: string;
  /** 찾기용 덧붙임(브랜치·window 이름 등) */
  detail?: string;
  /** 오른쪽에 보이는 단축키 */
  keys?: string;
  run: () => void;
}

export function Palette(props: { commands: Command[]; onClose: () => void }) {
  const { commands, onClose } = props;
  const [q, setQ] = useState("");
  const [at, setAt] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  // 열기 전에 포커스가 있던 곳 — 닫을 때 돌려준다.
  const before = useRef<Element | null>(document.activeElement);

  useEffect(() => {
    input.current?.focus();
  }, []);

  const shown = useMemo(() => {
    const words = q.toLowerCase().split(/\s+/).filter(Boolean);
    return commands.filter((c) => {
      const hay = `${c.label} ${c.detail ?? ""}`.toLowerCase();
      return words.every((w) => hay.includes(w));
    });
  }, [q, commands]);

  useEffect(() => setAt(0), [q]);

  const close = (then?: () => void) => {
    onClose();
    (before.current as HTMLElement | null)?.focus?.();
    then?.();
  };

  return (
    <div className="modal-back palette-back" onMouseDown={() => close()}>
      <div className="palette" role="dialog" aria-label="명령" onMouseDown={(e) => e.stopPropagation()}>
        <input
          ref={input}
          value={q}
          placeholder="세션·동작 찾기"
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") close();
            else if (e.key === "ArrowDown") setAt((i) => Math.min(shown.length - 1, i + 1));
            else if (e.key === "ArrowUp") setAt((i) => Math.max(0, i - 1));
            else if (e.key === "Enter" && shown[at]) close(shown[at].run);
            else return;
            e.preventDefault();
          }}
        />
        <ul>
          {shown.map((c, i) => (
            <li
              key={c.id}
              className={i === at ? "active" : ""}
              onMouseEnter={() => setAt(i)}
              onMouseDown={(e) => {
                e.preventDefault();
                close(c.run);
              }}
            >
              <span>{c.label}</span>
              {c.detail && <span className="muted">{c.detail}</span>}
              {c.keys && <kbd>{c.keys}</kbd>}
            </li>
          ))}
          {shown.length === 0 && <li className="muted">없음</li>}
        </ul>
      </div>
    </div>
  );
}
