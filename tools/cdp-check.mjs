// Debug-only helper: drives the flyout through the WebView2 DevTools protocol
// so UI checks do not need synthetic mouse input on the real desktop.
// Requires the app to run with WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

const port = process.env.CDP_PORT ?? "9222";
const title = process.env.CDP_TITLE ?? "LumenTray";
const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
const page = targets.find((target) => target.type === "page" && target.title === title);
if (!page) throw new Error(`flyout page "${title}" not found on debug port ${port}`);

const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
  ws.addEventListener("open", resolve, { once: true });
  ws.addEventListener("error", reject, { once: true });
});

let sequence = 0;
const pending = new Map();
ws.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  const entry = pending.get(message.id);
  if (!entry) return;
  pending.delete(message.id);
  entry(message);
});

function send(method, params = {}) {
  const id = ++sequence;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      if (pending.delete(id)) reject(new Error(`timeout: ${method}`));
    }, 30000);
    pending.set(id, (message) => {
      clearTimeout(timer);
      if (message.error) reject(new Error(JSON.stringify(message.error)));
      else resolve(message.result);
    });
  });
}

async function evaluate(expression) {
  const result = await send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails) {
    const detail = result.exceptionDetails.exception?.description ?? result.exceptionDetails.text;
    throw new Error(detail);
  }
  return result.result.value;
}

const readState = `(() => ({
  title: document.querySelector("h1")?.textContent ?? null,
  master: document.querySelector(".master-block .value")?.textContent ?? null,
  masterDisabled: document.querySelector(".master-block .slider")?.disabled ?? null,
  cards: [...document.querySelectorAll(".monitor")].map((card) => ({
    name: card.querySelector(".monitor-name")?.textContent ?? null,
    meta: card.querySelector(".monitor-meta")?.textContent ?? null,
    value: card.querySelector(".value")?.textContent ?? null,
    slider: card.querySelector(".slider")?.value ?? null,
    hint: card.querySelector(".monitor-error")?.textContent ?? null,
  })),
  status: document.querySelector(".status")?.textContent ?? null,
  notes: document.querySelector(".status")?.title ?? null,
  loading: Boolean(document.querySelector(".skeleton-list")),
}))()`;

const report = {};

report.state = await evaluate(readState);

await evaluate(`document.querySelector(".titlebar .icon-button").click(); true`);
await new Promise((resolve) => setTimeout(resolve, 2500));
report.afterRefresh = await evaluate(readState);

report.backendWrite = await evaluate(`(async () => {
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const list = await invoke("list_monitors");
  const results = [];
  for (const monitor of list.monitors) {
    if (!monitor.brightness) continue;
    const started = performance.now();
    try {
      const outcome = await invoke("set_brightness", {
        id: monitor.id,
        percent: monitor.brightness.percent,
      });
      results.push({
        name: monitor.name || monitor.gdiName,
        percent: monitor.brightness.percent,
        actual: outcome.brightness?.percent ?? null,
        ok: !outcome.error,
        error: outcome.error ?? null,
        ms: Math.round(performance.now() - started),
      });
    } catch (error) {
      results.push({ name: monitor.name || monitor.gdiName, ok: false, error });
    }
  }
  return results;
})()`);

const shot = await send("Page.captureScreenshot", { format: "png", fromSurface: true });
const shotPath = process.argv[2] ?? "cdp-check.png";
mkdirSync(dirname(shotPath), { recursive: true });
writeFileSync(shotPath, Buffer.from(shot.data, "base64"));
report.screenshot = shotPath;

const logPath = process.argv[3];
if (logPath) {
  mkdirSync(dirname(logPath), { recursive: true });
  writeFileSync(logPath, JSON.stringify(report, null, 2));
}

console.log(JSON.stringify(report, null, 2));
ws.close();
