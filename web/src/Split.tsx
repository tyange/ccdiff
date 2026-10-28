// 칸 사이 경계선: 끌어서 크기를 바꾸고, 더블클릭하면 기본값으로 돌아간다. 크기는 이 브라우저에 기억한다.

import { useState } from "react";

/** 브라우저 저장소에 남기는 숫자 하나. 저장소를 못 쓰면(사생활 보호 창 등) 기억만 못 할 뿐 동작한다. */
export function useStored(key: string, initial: number): [number, (v: number) => void] {
  const [v, setV] = useState(() => {
    try {
      const s = localStorage.getItem(`worktable:${key}`);
      const n = s === null ? NaN : Number(s);
      return Number.isFinite(n) ? n : initial;
    } catch {
      return initial;
    }
  });
  const set = (n: number) => {
    setV(n);
    try {
      localStorage.setItem(`worktable:${key}`, String(n));
    } catch {
      // 기억 못 해도 크기는 바뀐다.
    }
  };
  return [v, set];
}

export const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** dir="x" 는 좌우로 끄는 세로 경계선, dir="y" 는 위아래로 끄는 가로 경계선. onDrag 는 포인터의 화면 좌표를 받는다. */
export function Split(props: { dir: "x" | "y"; onDrag: (x: number, y: number) => void; onReset: () => void; className?: string }) {
  const { dir, onDrag, onReset, className } = props;
  return (
    <div
      className={`split ${dir}${className ? ` ${className}` : ""}`}
      role="separator"
      aria-orientation={dir === "x" ? "vertical" : "horizontal"}
      onPointerDown={(e) => {
        // 키보드 포커스는 원래 칸(터미널)에 남긴다.
        e.preventDefault();
        const el = e.currentTarget;
        el.setPointerCapture(e.pointerId);
        document.body.classList.add(`dragging-${dir}`);
        const move = (m: PointerEvent) => onDrag(m.clientX, m.clientY);
        const up = () => {
          el.removeEventListener("pointermove", move);
          el.removeEventListener("pointerup", up);
          el.removeEventListener("pointercancel", up);
          document.body.classList.remove(`dragging-${dir}`);
        };
        el.addEventListener("pointermove", move);
        el.addEventListener("pointerup", up);
        el.addEventListener("pointercancel", up);
      }}
      onDoubleClick={onReset}
    />
  );
}
