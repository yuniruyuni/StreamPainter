import ReactDOM from "react-dom/client";
import { OverlayApp } from "./Overlay";

// OBS Browser Source 専用。操作 UI やルーティングは持たず、常に透明 overlay を描画する。
document.documentElement.style.background = "transparent";
document.body.style.background = "transparent";

const root = document.getElementById("root");
if (root) {
  // StrictMode は開発時に effect を二重実行して WS を二重接続させるため使わない。
  ReactDOM.createRoot(root).render(<OverlayApp />);
}
