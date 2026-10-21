// Wires Rust events and commands to the screen, log, and footer (U9).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";

import { view } from "./copy";
import { Screen } from "./render";
import type { Action, AppState, LogLine } from "./types";
import "./styles.css";

const LOG_CAP = 500;
const TOAST_MS = 2000;

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const logBox = $<HTMLDivElement>("log");
const logToggle = $<HTMLButtonElement>("log-toggle");
const toast = $<HTMLDivElement>("toast");
let toastTimer: number | undefined;

function stateKey(s: AppState): string {
  const parts: string[] = [s.kind];
  if ("attempt" in s) parts.push(String(s.attempt));
  if ("message" in s) parts.push(s.message);
  if ("devices" in s) parts.push(String(s.devices.length));
  return parts.join("|");
}

function appendLog(line: LogLine): void {
  const atBottom = logBox.scrollHeight - logBox.scrollTop - logBox.clientHeight < 8;
  const row = document.createElement("div");
  row.className = `line ${line.stream}`;
  row.textContent = line.text;
  logBox.append(row);
  while (logBox.childElementCount > LOG_CAP) logBox.firstElementChild?.remove();
  if (atBottom) logBox.scrollTop = logBox.scrollHeight;
}

function setLogOpen(open: boolean): void {
  logToggle.setAttribute("aria-expanded", String(open));
  logToggle.textContent = open ? "Show log ▾" : "Show log ▸";
  logBox.hidden = !open;
  if (open) logBox.scrollTop = logBox.scrollHeight;
}

function showToast(text: string): void {
  toast.textContent = text;
  toast.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (toast.hidden = true), TOAST_MS);
}

async function copyDiagnostics(): Promise<void> {
  const text = await invoke<string>("get_diagnostics");
  await writeText(text);
  showToast("Copied");
}

function act(a: Action): void {
  const run = async () => {
    switch (a) {
      case "copy_diagnostics":
        return copyDiagnostics();
      case "show_log":
        return setLogOpen(true);
      default:
        return invoke(a);
    }
  };
  run().catch((e) => appendLog({ ts: new Date().toISOString(), stream: "app", text: `error: ${String(e)}` }));
}

const screen = new Screen($("app"), act);
const render = (s: AppState) => screen.show(view(s), stateKey(s));

logToggle.addEventListener("click", () => setLogOpen(logToggle.getAttribute("aria-expanded") !== "true"));
$("copy-diagnostics").addEventListener("click", () => act("copy_diagnostics"));
$("reveal-backups").addEventListener("click", () => act("reveal_backups"));

async function start(): Promise<void> {
  // Subscribe before the first read so no event falls in the gap.
  await listen<AppState>("state-changed", (e) => render(e.payload));
  await listen<LogLine>("log-line", (e) => appendLog(e.payload));
  render(await invoke<AppState>("get_state"));
  for (const line of await invoke<LogLine[]>("get_log")) appendLog(line);
}

render({ kind: "starting" });
start().catch((e) => console.error(e));
