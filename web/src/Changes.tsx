// 변경 칸: 세션이 고친 파일 목록(✓◐●✗◆)과 Monaco 읽기 전용 diff.
// 수정 후 쪽에서 줄을 마우스로 고르면 손을 뗄 때 그 세션의 Claude 에 보내고, 키보드 포커스는 터미널로 돌려준다.

import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";

import { postJson } from "./api";
import { monaco } from "./monaco";
import { clamp, Split, useStored } from "./Split";
import type { ChangeItem, DiffResult, Mark, Session } from "./types";

export interface ChangesHandle {
  /** 목록에서 위(-1)·아래(+1) 파일 */
  stepFile(d: number): void;
  /** 같은 파일의 이전(-1)·다음(+1) 수정 */
  stepEdit(d: number): void;
  openInEditor(): void;
}

const MARK: Record<Mark, { sym: string; title: string }> = {
  committed: { sym: "✓", title: "커밋됨" },
  changed: { sym: "◐", title: "커밋 전에 더 고침" },
  uncommitted: { sym: "●", title: "미커밋" },
  gone: { sym: "✗", title: "커밋 없이 지워짐" },
  worktree: { sym: "◆", title: "작업 트리(HEAD 대비, Bash 수정 포함)" },
  unknown: { sym: "·", title: "판정 전" },
};

interface FileRow {
  path: string;
  rel: string;
  worktree: string;
  /** 시간순 항목 — 마지막이 가장 최근 */
  items: ChangeItem[];
}

function hhmm(epoch: number): string {
  if (!epoch) return "";
  const d = new Date(epoch * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

const prefersDark = () => window.matchMedia("(prefers-color-scheme: dark)").matches;

interface Sent {
  lines: number;
  claude: number;
}

export const Changes = forwardRef<ChangesHandle, { session: Session | null; focusTerminal: () => void }>(
  function Changes({ session, focusTerminal }, ref) {
    const [items, setItems] = useState<ChangeItem[]>([]);
    const [file, setFile] = useState<string | null>(null);
    const [itemId, setItemId] = useState<string | null>(null);
    const [diff, setDiff] = useState<DiffResult | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [sideBySide, setSideBySide] = useState(false);
    const [collapse, setCollapse] = useState(true);
    const [sent, setSent] = useState<Sent | null>(null);
    const [notice, setNotice] = useState<string | null>(null);
    const box = useRef<HTMLDivElement>(null);
    const root = useRef<HTMLDivElement>(null);
    const [filesH, setFilesH] = useStored("changes-files-h", 160);
    const editor = useRef<monaco.editor.IStandaloneDiffEditor | null>(null);
    const key = session?.key ?? null;

    // 목록: 세션이 바뀌거나 새 수정·커밋이 생기면, 그리고 작업 트리 변경을 보려고 10초마다.
    const trigger = session ? `${session.key}:${session.edits}:${session.uncommitted}:${session.lastActivity}` : "";
    useEffect(() => {
      if (!key) {
        setItems([]);
        return;
      }
      let live = true;
      const load = () =>
        postJson<ChangeItem[]>("/api/changes", { key }).then(
          (x) => live && setItems(x),
          (e: Error) => live && setError(e.message),
        );
      load();
      const t = window.setInterval(load, 10_000);
      return () => {
        live = false;
        window.clearInterval(t);
      };
    }, [trigger]);

    // 세션을 바꾸면 고른 파일·선택을 비운다.
    useEffect(() => {
      setFile(null);
      setItemId(null);
      setSent(null);
    }, [key]);

    // 파일 단위로 묶어 최근 순. 워크트리 이름은 묶음 머리줄에 쓴다.
    const files = useMemo(() => {
      const byPath = new Map<string, FileRow>();
      for (const it of items) {
        const f = byPath.get(it.path) ?? { path: it.path, rel: it.rel, worktree: it.worktree ?? "", items: [] };
        f.items.push(it);
        byPath.set(it.path, f);
      }
      const last = (f: FileRow) => Math.max(...f.items.map((i) => i.epoch || 0));
      // 최근에 손댄 워크트리 묶음이 위로, 묶음 안에서는 최근 파일이 위로.
      const groupLast = new Map<string, number>();
      for (const f of byPath.values()) groupLast.set(f.worktree, Math.max(groupLast.get(f.worktree) ?? 0, last(f)));
      return [...byPath.values()].sort(
        (a, b) =>
          groupLast.get(b.worktree)! - groupLast.get(a.worktree)! || a.worktree.localeCompare(b.worktree) || last(b) - last(a),
      );
    }, [items]);

    // 파일을 고르면 그 파일의 가장 최근 항목.
    useEffect(() => {
      if (!file) return;
      const f = files.find((x) => x.path === file);
      if (f && !f.items.some((i) => i.id === itemId)) setItemId(f.items[f.items.length - 1].id);
    }, [file, files]);

    useEffect(() => {
      if (!key || !itemId) {
        setDiff(null);
        return;
      }
      let live = true;
      postJson<DiffResult>("/api/diff", { key, id: itemId }).then(
        (d) => live && (setDiff(d), setError(null)),
        (e: Error) => live && setError(e.message),
      );
      return () => {
        live = false;
      };
    }, [key, itemId]);

    // Monaco diff 편집기는 한 번 만든다.
    useEffect(() => {
      const ed = monaco.editor.createDiffEditor(box.current!, {
        readOnly: true,
        originalEditable: false,
        automaticLayout: true,
        renderSideBySide: false,
        minimap: { enabled: false },
        scrollBeyondLastLine: false,
        fontSize: 12,
        renderOverviewRuler: false,
        theme: prefersDark() ? "vs-dark" : "vs",
      });
      editor.current = ed;
      const mq = window.matchMedia("(prefers-color-scheme: dark)");
      const onScheme = () => monaco.editor.setTheme(mq.matches ? "vs-dark" : "vs");
      mq.addEventListener("change", onScheme);
      return () => {
        mq.removeEventListener("change", onScheme);
        ed.getModel()?.original.dispose();
        ed.getModel()?.modified.dispose();
        ed.dispose();
      };
    }, []);

    useEffect(() => {
      editor.current?.updateOptions({ renderSideBySide: sideBySide, hideUnchangedRegions: { enabled: collapse } });
    }, [sideBySide, collapse]);

    // 내용 바꾸기: 경로로 모델을 만들어 확장자로 언어를 고르게 한다.
    const generation = useRef(0);
    useEffect(() => {
      const ed = editor.current;
      if (!ed) return;
      const old = ed.getModel();
      if (diff && !diff.secret) {
        const n = ++generation.current;
        const uri = (side: string) => monaco.Uri.from({ scheme: "inmemory", path: `/${n}/${side}${diff.info.path}` });
        ed.setModel({
          original: monaco.editor.createModel(diff.original, undefined, uri("a")),
          modified: monaco.editor.createModel(diff.modified, undefined, uri("b")),
        });
        // 첫 변경 줄로.
        if (diff.firstChange) ed.getModifiedEditor().revealLineInCenter(diff.firstChange);
      } else {
        ed.setModel(null);
      }
      old?.original.dispose();
      old?.modified.dispose();
    }, [diff]);

    const current = diff && !diff.secret ? diff : null;
    const path = diff?.info.path ?? null;

    // 고른 줄 범위(1부터). 끝이 다음 줄 첫 칸이면 그 줄은 빼고 센다.
    const selectedLines = (): [number, number] | null => {
      const s = editor.current?.getModifiedEditor().getSelection();
      if (!s || s.isEmpty()) return null;
      const end = s.endColumn === 1 && s.endLineNumber > s.startLineNumber ? s.endLineNumber - 1 : s.endLineNumber;
      return [s.startLineNumber, end];
    };

    const sendSelection = async () => {
      const m = editor.current?.getModifiedEditor().getModel();
      const lines = selectedLines();
      if (!key || !path || !m || !lines) return;
      const text = m.getValueInRange(new monaco.Range(lines[0], 1, lines[1], m.getLineMaxColumn(lines[1])));
      try {
        const r = await postJson<{ sent: number }>("/api/selection", { key, path, start: lines[0], end: lines[1], text });
        setSent({ lines: lines[1] - lines[0] + 1, claude: r.sent });
      } catch (e) {
        setNotice((e as Error).message);
      }
    };

    const clearSelection = async () => {
      editor.current?.getModifiedEditor().setSelection(new monaco.Selection(1, 1, 1, 1));
      setSent(null);
      if (key) await postJson("/api/selection", { key, clear: true }).catch(() => {});
    };

    // 마우스로 고르고 손을 떼면 보내고 터미널로 돌아간다 — 고르고 바로 입력.
    const latest = useRef({ sendSelection, focusTerminal });
    latest.current = { sendSelection, focusTerminal };
    useEffect(() => {
      const ed = editor.current!.getModifiedEditor();
      const sub = ed.onMouseUp(() => {
        if (!ed.getSelection()?.isEmpty()) {
          void latest.current.sendSelection();
          latest.current.focusTerminal();
        }
      });
      return () => sub.dispose();
    }, []);

    const openInEditor = async () => {
      if (!key || !itemId) return;
      const line = selectedLines()?.[0] ?? (current?.firstChange || 1);
      try {
        await postJson("/api/open", { key, id: itemId, line });
        setNotice(`편집기로 열었다: ${diff?.info.rel}:${line}`);
      } catch (e) {
        setNotice((e as Error).message);
      }
    };

    useImperativeHandle(ref, () => ({
      stepFile(d) {
        if (files.length === 0) return;
        const i = files.findIndex((f) => f.path === file);
        const next = files[Math.min(files.length - 1, Math.max(0, i < 0 ? 0 : i + d))];
        setFile(next.path);
        setItemId(next.items[next.items.length - 1].id);
      },
      stepEdit(d) {
        const seq = diff?.info.seq ?? [];
        const i = seq.indexOf(itemId ?? "");
        if (i >= 0 && seq[i + d]) setItemId(seq[i + d]);
      },
      openInEditor,
    }));

    useEffect(() => {
      if (!notice) return;
      const t = window.setTimeout(() => setNotice(null), 4000);
      return () => window.clearTimeout(t);
    }, [notice]);

    const seq = diff?.info.seq ?? [];
    const pos = seq.indexOf(itemId ?? "");
    const item = items.find((i) => i.id === itemId);
    let lastWt = "";

    return (
      <div className="changes" ref={root} style={{ gridTemplateRows: `${filesH}px 6px auto minmax(0, 1fr) auto` }}>
        <div className="files">
          {files.map((f) => {
            const last = f.items[f.items.length - 1];
            const head = f.worktree !== lastWt ? <div className="wt-head">{(lastWt = f.worktree) || "(git 밖)"}</div> : null;
            return (
              <div key={f.path}>
                {head}
                <div
                  className={`file${f.path === file ? " selected" : ""}`}
                  title={MARK[last.mark].title + (last.commit ? ` ${last.commit.hash} ${last.commit.subject}` : "")}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    setFile(f.path);
                    setItemId(last.id);
                  }}
                >
                  <span className={`mark ${last.mark}`}>{MARK[last.mark].sym}</span>
                  {/* 긴 경로는 앞을 줄이려고 rtl 로 두고, 글자 순서는 안쪽 ltr 로 지킨다(.env 의 점이 뒤로 가지 않게). */}
                  <span className="name">
                    <span dir="ltr">{f.rel}</span>
                  </span>
                  {f.items.length > 1 && <span className="count">×{f.items.length}</span>}
                  <span className="plus">+{last.added}</span>
                  <span className="minus">-{last.removed}</span>
                  <span className="time">{hhmm(last.epoch)}</span>
                </div>
              </div>
            );
          })}
        </div>
        <Split
          dir="y"
          onDrag={(_, y) => {
            const r = root.current?.getBoundingClientRect();
            if (r) setFilesH(clamp(y - r.top, 40, r.height - 120));
          }}
          onReset={() => setFilesH(160)}
        />
        <div className="diff-head">
          {diff ? (
            <>
              <span className="rel" title={diff.info.path}>
                <span dir="ltr">{diff.info.rel}</span>
              </span>
              {item && <span className={`mark ${item.mark}`}>{MARK[item.mark].sym}</span>}
              {seq.length > 1 && (
                <span className="steps">
                  <button disabled={pos <= 0} onClick={() => setItemId(seq[pos - 1])} title="이전 수정 (⌘⇧↑)">
                    ◀
                  </button>
                  수정 {pos + 1}/{seq.length}
                  <button disabled={pos >= seq.length - 1} onClick={() => setItemId(seq[pos + 1])} title="다음 수정 (⌘⇧↓)">
                    ▶
                  </button>
                </span>
              )}
              {current?.partial && (
                <span className="muted" title="수정 전 파일이 기록에 없어 바뀐 부분만 보인다">
                  일부
                </span>
              )}
              <span className="spacer" />
              <label>
                <input type="checkbox" checked={collapse} onChange={(e) => setCollapse(e.target.checked)} /> 접기
              </label>
              <label>
                <input type="checkbox" checked={sideBySide} onChange={(e) => setSideBySide(e.target.checked)} /> 좌우
              </label>
              <button onClick={openInEditor} title="데스크톱 편집기에서 이 줄 열기 (⌘E)">
                VS Code ⌘E
              </button>
            </>
          ) : null}
        </div>
        <div className="diff-box">
          {diff?.secret && (
            <p className="empty" title="시크릿 파일은 내용을 띄우지 않는다">
              시크릿
            </p>
          )}
          <div className="monaco" ref={box} style={{ visibility: current ? "visible" : "hidden" }} />
        </div>
        <div className="sel-bar">
          {session && (
            <span
              className={session.ide ? "ide on" : "ide off"}
              title={session.ide ? "Claude 가 worktable IDE 에 붙어 있다" : "Claude 에서 /ide 로 worktable 을 고르면 줄 선택을 보낼 수 있다"}
            >
              IDE
            </span>
          )}
          {sent && (
            <>
              <span className={sent.claude === 0 ? "warn" : ""}>⧉ {sent.lines}줄</span>
              <button className="link" onClick={clearSelection}>
                지우기
              </button>
            </>
          )}
          {notice && <span className="notice">{notice}</span>}
          {error && <span className="warn">{error}</span>}
        </div>
      </div>
    );
  },
);
