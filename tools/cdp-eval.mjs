// Debug-only helper: evaluate a JS expression inside the running flyout.
// Usage: node tools/cdp-eval.mjs - < expression.js   (or pass a file path)
// Env: CDP_DARK=1 emulate prefers-color-scheme: dark; CDP_SCREENSHOT=path save a PNG.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

const source = process.argv[2] === "-" ? readFileSync(0, "utf8") : readFileSync(process.argv[2], "utf8");
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

if (process.env.CDP_DARK || process.env.CDP_LIGHT) {
  await send("Emulation.setEmulatedMedia", {
    features: [
      { name: "prefers-color-scheme", value: process.env.CDP_DARK ? "dark" : "light" },
    ],
  });
  // let CSS transitions settle before capturing
  await new Promise((resolve) => setTimeout(resolve, 350));
}

const result = await send("Runtime.evaluate", {
  expression: source,
  awaitPromise: true,
  returnByValue: true,
});
if (result.exceptionDetails) {
  console.error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
  process.exitCode = 1;
} else {
  console.log(JSON.stringify(result.result.value, null, 2));
}

if (process.env.CDP_SCREENSHOT) {
  const shot = await send("Page.captureScreenshot", { format: "png", fromSurface: true });
  mkdirSync(dirname(process.env.CDP_SCREENSHOT), { recursive: true });
  writeFileSync(process.env.CDP_SCREENSHOT, Buffer.from(shot.data, "base64"));
}
ws.close();
