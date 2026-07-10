import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
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

interface LibraryItem {
  id: string;
  name: string;
  kind: "video" | "image";
  file: string;
  preview: string | null;
  imported_at: number;
}

const el = <T extends HTMLElement>(id: string): T => {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element #${id}`);
  return node as T;
};

const daemonState = el<HTMLSpanElement>("daemon-state");
const connectButton = el<HTMLButtonElement>("connect");
const autostartCheckbox = el<HTMLInputElement>("autostart");
const batterySelect = el<HTMLSelectElement>("battery");
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
    await refreshLibrary();
    try {
      autostartCheckbox.checked = await invoke<boolean>("get_autostart");
      autostartCheckbox.disabled = false;
    } catch {
      autostartCheckbox.disabled = true;
    }
    try {
      batterySelect.value = await invoke<string>("get_battery_policy");
      batterySelect.disabled = false;
    } catch {
      batterySelect.disabled = true;
    }
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

const libraryBox = el<HTMLDivElement>("library");
const previewCache = new Map<string, string>();

async function refreshLibrary() {
  const items = await call<LibraryItem[]>("library_list");
  libraryBox.replaceChildren();
  if (items.length === 0) {
    const hint = document.createElement("p");
    hint.className = "hint";
    hint.textContent = "Library is empty.";
    libraryBox.append(hint);
    return;
  }
  items.sort((a, b) => b.imported_at - a.imported_at);
  for (const item of items) {
    libraryBox.append(renderCard(item));
  }
}

function renderCard(item: LibraryItem): HTMLElement {
  const card = document.createElement("div");
  card.className = "card";
  card.title = item.file;

  const thumb = document.createElement("div");
  thumb.className = "thumb";
  if (item.preview) {
    const img = document.createElement("img");
    const cached = previewCache.get(item.id);
    if (cached) {
      img.src = cached;
    } else {
      void call<string>("library_preview", { id: item.id })
        .then((data) => {
          const url = `data:image/jpeg;base64,${data}`;
          previewCache.set(item.id, url);
          img.src = url;
        })
        .catch(() => {});
    }
    thumb.append(img);
  } else {
    thumb.textContent = item.kind === "video" ? "video" : "image";
  }
  card.append(thumb);

  const name = document.createElement("div");
  name.className = "name";
  name.textContent = item.name;
  card.append(name);

  const actions = document.createElement("div");
  actions.className = "card-actions";
  const applyButton = document.createElement("button");
  applyButton.textContent = "Apply";
  applyButton.className = "primary";
  applyButton.addEventListener("click", () => void applyLibraryItem(item));
  const removeButton = document.createElement("button");
  removeButton.textContent = "✕";
  removeButton.title = "Remove from library";
  removeButton.addEventListener("click", () => {
    void call<void>("library_remove", { id: item.id }).then(() => {
      previewCache.delete(item.id);
      void refreshLibrary();
    });
  });
  actions.append(applyButton, removeButton);
  card.append(actions);
  return card;
}

async function applyLibraryItem(item: LibraryItem) {
  fileInput.value = item.file;
  const status = await call<string>("play", {
    path: item.file,
    monitor: selectedMonitor,
    quality: qualitySelect.value,
    volume: Number(volumeRange.value),
    anime4k: anime4kCheckbox.checked,
  });
  report(status);
  await refreshSessions();
}

async function importPaths(paths: string[]) {
  for (const path of paths) {
    report(`importing ${path}…`);
    try {
      const item = await call<LibraryItem>("library_import", { path });
      report(`imported ${item.name}`);
    } catch {
      // error already reported by call()
    }
  }
  await refreshLibrary();
}

async function importDialog() {
  const picked = await open({
    multiple: true,
    filters: [
      {
        name: "Media",
        extensions: [
          "mp4",
          "mkv",
          "webm",
          "mov",
          "avi",
          "m4v",
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
  if (Array.isArray(picked)) {
    await importPaths(picked);
  } else if (typeof picked === "string") {
    await importPaths([picked]);
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
  el<HTMLButtonElement>("import").addEventListener("click", () => void importDialog());
  void getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "drop") {
      void importPaths(event.payload.paths);
    }
  });
  el<HTMLButtonElement>("browse").addEventListener("click", () => void browse());
  el<HTMLButtonElement>("apply").addEventListener("click", () => void apply());
  el<HTMLButtonElement>("pause").addEventListener("click", () => void simple("pause"));
  el<HTMLButtonElement>("resume").addEventListener("click", () => void simple("resume"));
  el<HTMLButtonElement>("stop").addEventListener("click", () => void simple("stop"));
  autostartCheckbox.addEventListener("change", () => {
    void call<string>("set_autostart", { enabled: autostartCheckbox.checked })
      .then((status) => report(status))
      .catch(() => {
        autostartCheckbox.checked = !autostartCheckbox.checked;
      });
  });
  batterySelect.addEventListener("change", () => {
    void call<string>("set_battery_policy", { policy: batterySelect.value })
      .then((status) => report(status))
      .catch(() => {});
  });
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
