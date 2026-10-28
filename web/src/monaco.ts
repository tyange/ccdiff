// 읽기 전용 diff 에 필요한 것만 가져온다: 편집기 API, diff 편집기, 찾기, 문법 색칠(monarch).
// 언어 서비스(TypeScript 워커 등)는 넣지 않는다 — import 를 못 찾는다는 빨간 줄만 생기고 쓸 일이 없다.
// monaco-editor 0.57 은 exports 로 `monaco-editor/<경로>.js` → `esm/vs/<경로>.js` 만 열어 두어 CSS 는 파일 경로로 가져온다.

import "monaco-editor/editor/browser/widget/diffEditor/diffEditor.contribution.js";
import "monaco-editor/features/find/register.js";
import "monaco-editor/basic-languages/monaco.contribution.js";
import "../node_modules/monaco-editor/esm/vs/base/browser/ui/codicons/codicon/codicon.css";
import * as monaco from "monaco-editor/editor/editor.api.js";
import EditorWorker from "monaco-editor/editor/editor.worker.js?worker";

// diff 계산은 editor 워커가 한다.
self.MonacoEnvironment = { getWorker: () => new EditorWorker() };

export { monaco };
