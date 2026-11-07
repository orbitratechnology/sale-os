import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { mockIPC } from "@tauri-apps/api/mocks";
import { App } from "../../src/App";
import { handle, initialState, type FakeState, type Preset } from "./fakeBackend";
import "../../src/styles.css";

declare global {
  interface Window {
    __ipc: { cmd: string; args: Record<string, unknown> }[];
    __fake: FakeState;
  }
}

const preset = (new URLSearchParams(location.search).get("preset") ?? "fresh") as Preset;
window.__fake = initialState(preset);
const big = new URLSearchParams(location.search).get("big");
if (big) window.__fake.bigCatalog = parseInt(big, 10);
// A live till with cash.require_session_for_sale: sales need an open drawer.
if (new URLSearchParams(location.search).get("drawer") === "required") window.__fake.drawer.required = true;
window.__ipc = [];
mockIPC((cmd, args) => {
  const a = (args ?? {}) as Record<string, unknown>;
  window.__ipc.push({ cmd, args: a });
  return handle(window.__fake, cmd, a);
});

const root = document.getElementById("root");
if (!root) throw new Error("#root missing");
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
