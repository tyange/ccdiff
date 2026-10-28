// 주소의 #t= 토큰을 탭 저장소로 옮기고 주소에서 지운다 — 방문 기록·화면 공유에 남지 않게.
function takeToken() {
  const m = location.hash.match(/(?:^#|&)t=([0-9a-f]+)/);
  try {
    if (m) {
      sessionStorage.setItem("worktable-token", m[1]);
      history.replaceState(null, "", location.pathname);
    }
    return sessionStorage.getItem("worktable-token") || (m && m[1]);
  } catch {
    return m && m[1];
  }
}

const out = document.getElementById("out");
const token = takeToken();
const lines = [];
const show = (s) => { lines.push(s); out.textContent = lines.join("\n"); };

fetch("/api/health", { headers: { Authorization: `Bearer ${token}` } })
  .then((r) => r.json().then((j) => show(`API ${r.status} ${JSON.stringify(j)}`)))
  .catch((e) => show(`API 실패: ${e}`));

const ws = new WebSocket(`ws://${location.host}/ws/events`, ["worktable.v1", `auth.${token}`]);
ws.onopen = () => show(`WS 연결 (프로토콜 ${ws.protocol})`);
ws.onmessage = (e) => show(`WS 받음 ${e.data}`);
ws.onerror = () => show("WS 오류");
