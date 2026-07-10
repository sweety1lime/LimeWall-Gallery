import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

interface Monitor {
  id: number;
  name: string;
  bounds: { x: number; y: number; width: number; height: number };
  scale: number;
  is_primary: boolean;
}

interface SessionStatus {
  monitor: number;
  state: "playing" | "paused" | "stopped";
  path: string | null;
  quality: "eco" | "balanced" | "max";
  volume: number;
  anime4k: boolean;
}

const el = <T extends HTMLElement>(id: string): T => {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element #${id}`);
  return node as T;
};

const daemonState = el<HTMLSpanElement>("daemon-state");
const connectButton = el<HTMLButtonElement>("connect");
const monitorsBox = el<HTMLDivElement>("monitors");
const sessionsBox = el<HTMLDivElement>("sessions");
const fileInput = el<HTMLInputElement>("file");
const qualitySelect = el<HTMLSelectElement>("quality");
const anime4kCheckbox = el<HTMLInputElement>("anime4k");
const volumeRange = el<HTMLInputElement>("volume");
const volumeValue = el<HTMLSpanElement>("volume-value");
const messageBox = el<HTMLParagraphElement>("message");

let connected = false;
let selectedMonitor = 0;
let refreshTimer: number | undefined;

function report(text: string, isError = false) {
  messageBox.textContent = text;
  messageBox.classList.toggle("error", isError);
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    report(String(error), true);
    throw error;
  }
}

function setConnected(version: string | null) {
  connected = version !== null;
  daemonState.textContent = connected ? `online v${version}` : "offline";
  daemonState.classList.toggle("online", connected);
  daemonState.classList.toggle("offline", !connected);
  if (connected && refreshTimer === undefined) {
    refreshTimer = window.setInterval(() => {
      refreshSessions().catch(() => setDisconnected());
    }, 3000);
  }
}

function setDisconnected() {
  if (refreshTimer !== undefined) {
    window.clearInterval(refreshTimer);
    refreshTimer = undefined;
  }
  setConnected(null);
}

async function connect() {
  connectButton.disabled = true;
  try {
    const version = await call<string>("daemon_connect");
    setConnected(version);
    report("connected to renderer daemon");
    await refreshMonitors();
    await refreshSessions();
  } catch {
    setDisconnected();
  } finally {
    connectButton.disabled = false;
  }
}

async function refreshMonitors() {
  const monitors = await call<Monitor[]>("list_monitors");
  monitorsBox.replaceChildren();
  if (monitors.length === 0) {
    monitorsBox.textContent = "no monitors detected";
    return;
  }
  if (!monitors.some((monitor) => monitor.id === selectedMonitor)) {
    selectedMonitor = monitors[0].id;
  }
  for (const monitor of monitors) {
    const label = document.createElement("label");
    label.className = "monitor";
    const radio = document.createElement("input");
    radio.type = "radio";
    radio.name = "monitor";
    radio.value = String(monitor.id);
    radio.checked = monitor.id === selectedMonitor;
    radio.addEventListener("change", () => {
      selectedMonitor = monitor.id;
    });
    const text = document.createElement("span");
    text.textContent =
      `${monitor.id}: ${monitor.name}  ${monitor.bounds.width}x${monitor.bounds.height}` +
      (monitor.is_primary ? "  (primary)" : "");
    label.append(radio, text);
    monitorsBox.append(label);
  }
}

async function refreshSessions() {
  const sessions = await call<SessionStatus[]>("daemon_status");
  sessionsBox.replaceChildren();
  if (sessions.length === 0) {
    const hint = document.createElement("p");
    hint.className = "hint";
    hint.textContent = "Nothing is playing.";
    sessionsBox.append(hint);
    return;
  }
  for (const session of sessions) {
    const row = document.createElement("div");
    row.className = "session";
    const anime4k = session.anime4k ? " + anime4k" : "";
    row.textContent =
      `monitor ${session.monitor}: ${session.state}  ` +
      `${session.path ?? "-"}  [${session.quality}${anime4k}, volume ${session.volume}]`;
    sessionsBox.append(row);
  }
}

async function browse() {
  const picked = await open({
    multiple: false,
    filters: [
      {
        name: "Media",
        extensions: [
          "mp4",
          "mkv",
          "webm",
          "mov",
          "avi",
          "gif",
          "png",
          "jpg",
          "jpeg",
          "bmp",
          "webp",
        ],
      },
    ],
  });
  if (typeof picked === "string") {
    fileInput.value = picked;
  }
}

async function apply() {
  const path = fileInput.value.trim();
  if (!path) {
    report("choose a media file first", true);
    return;
  }
  const status = await call<string>("play", {
    path,
    monitor: selectedMonitor,
    quality: qualitySelect.value,
    volume: Number(volumeRange.value),
    anime4k: anime4kCheckbox.checked,
  });
  report(status);
  await refreshSessions();
}

async function simple(command: "pause" | "resume" | "stop") {
  const status = await call<string>(command, { monitor: selectedMonitor });
  report(status);
  await refreshSessions();
}

window.addEventListener("DOMContentLoaded", () => {
  connectButton.addEventListener("click", () => void connect());
  el<HTMLButtonElement>("browse").addEventListener("click", () => void browse());
  el<HTMLButtonElement>("apply").addEventListener("click", () => void apply());
  el<HTMLButtonElement>("pause").addEventListener("click", () => void simple("pause"));
  el<HTMLButtonElement>("resume").addEventListener("click", () => void simple("resume"));
  el<HTMLButtonElement>("stop").addEventListener("click", () => void simple("stop"));
  volumeRange.addEventListener("input", () => {
    volumeValue.textContent = volumeRange.value;
  });
  volumeRange.addEventListener("change", () => {
    if (!connected) return;
    void call<string>("set_volume", {
      monitor: selectedMonitor,
      volume: Number(volumeRange.value),
    })
      .then((status) => report(status))
      .catch(() => {});
  });
  qualitySelect.addEventListener("change", () => {
    if (!connected) return;
    void call<string>("set_quality", {
      monitor: selectedMonitor,
      quality: qualitySelect.value,
      anime4k: anime4kCheckbox.checked,
    })
      .then((status) => report(status))
      .catch(() => {});
  });
  void connect();
});
