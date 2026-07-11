import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";

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
  author?: string | null;
  license?: string | null;
}

const el = <T extends HTMLElement>(id: string): T => {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element #${id}`);
  return node as T;
};

const daemonState = el<HTMLSpanElement>("daemon-state");
const daemonText = el<HTMLSpanElement>("daemon-text");
const connectButton = el<HTMLButtonElement>("connect");
const autostartCheckbox = el<HTMLInputElement>("autostart");
const batterySelect = el<HTMLSelectElement>("battery");
const monitorsBox = el<HTMLSpanElement>("monitors");
const monitorOption = el<HTMLLabelElement>("monitor-option");
const nowPlaying = el<HTMLElement>("now-playing");
const sessionsBox = el<HTMLDivElement>("sessions");
const libraryBox = el<HTMLDivElement>("library");
const dropzone = el<HTMLDivElement>("dropzone");
const importButton = el<HTMLButtonElement>("import");
const qualitySelect = el<HTMLSelectElement>("quality");
const anime4kCheckbox = el<HTMLInputElement>("anime4k");
const volumeRange = el<HTMLInputElement>("volume");
const volumeValue = el<HTMLSpanElement>("volume-value");
const messageBox = el<HTMLParagraphElement>("message");

let connected = false;
let selectedMonitor = 0;
let refreshTimer: number | undefined;
let libraryItems: LibraryItem[] = [];
let activeSessions: SessionStatus[] = [];
const previewCache = new Map<string, string>();

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

// Windows canonical paths carry a \\?\ prefix; the library stores plain ones.
function normalizePath(path: string): string {
  return path.replace(/^\\\\\?\\/, "").toLowerCase();
}

function itemForPath(path: string | null): LibraryItem | undefined {
  if (!path) return undefined;
  const wanted = normalizePath(path);
  return libraryItems.find((item) => normalizePath(item.file) === wanted);
}

function fileStem(path: string): string {
  const base = path.split(/[\\/]/).pop() ?? path;
  return base.replace(/\.[^.]+$/, "");
}

function setConnected(version: string | null) {
  connected = version !== null;
  daemonText.textContent = connected ? "подключено" : "нет связи";
  daemonState.classList.toggle("online", connected);
  daemonState.classList.toggle("offline", !connected);
  connectButton.hidden = connected;
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
    report("");
    await refreshMonitors();
    await refreshLibrary();
    await refreshSessions();
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
    report("Не удалось запустить фоновый плеер. Нажмите «Подключить», чтобы повторить.", true);
  } finally {
    connectButton.disabled = false;
  }
}

async function refreshMonitors() {
  const monitors = await call<Monitor[]>("list_monitors");
  monitorsBox.replaceChildren();
  if (monitors.length === 0) return;
  if (!monitors.some((monitor) => monitor.id === selectedMonitor)) {
    selectedMonitor = monitors[0].id;
  }
  // One monitor: nothing to choose, hide the whole row.
  monitorOption.hidden = monitors.length < 2;
  for (const monitor of monitors) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "chip";
    chip.classList.toggle("selected", monitor.id === selectedMonitor);
    chip.textContent = `${monitor.id + 1}: ${monitor.bounds.width}×${monitor.bounds.height}`;
    chip.title = monitor.name + (monitor.is_primary ? " (основной)" : "");
    chip.addEventListener("click", () => {
      selectedMonitor = monitor.id;
      void refreshMonitors();
    });
    monitorsBox.append(chip);
  }
}

function sessionTitle(session: SessionStatus): string {
  const item = itemForPath(session.path);
  if (item) return item.name;
  return session.path ? fileStem(session.path) : "обои";
}

async function refreshSessions() {
  activeSessions = await call<SessionStatus[]>("daemon_status");
  nowPlaying.hidden = activeSessions.length === 0;
  sessionsBox.replaceChildren();
  for (const session of activeSessions) {
    sessionsBox.append(renderSession(session));
  }
  markActiveCards();
}

function renderSession(session: SessionStatus): HTMLElement {
  const row = document.createElement("div");
  row.className = "session-row";

  const item = itemForPath(session.path);
  const thumb = document.createElement("div");
  thumb.className = "session-thumb";
  if (item?.preview) {
    const img = document.createElement("img");
    void previewUrl(item.id).then((url) => {
      if (url) img.src = url;
    });
    thumb.append(img);
  }
  row.append(thumb);

  const info = document.createElement("div");
  info.className = "session-info";
  const title = document.createElement("div");
  title.className = "session-title";
  title.textContent = sessionTitle(session);
  const state = document.createElement("div");
  state.className = "session-state";
  const monitorPart = activeSessions.length > 1 ? `монитор ${session.monitor + 1} · ` : "";
  state.textContent = monitorPart + (session.state === "paused" ? "на паузе" : "играет");
  info.append(title, state);
  row.append(info);

  const controls = document.createElement("div");
  controls.className = "session-controls";
  const toggle = document.createElement("button");
  toggle.type = "button";
  if (session.state === "paused") {
    toggle.textContent = "▶ Продолжить";
    toggle.addEventListener("click", () => void sessionCommand("resume", session.monitor));
  } else {
    toggle.textContent = "⏸ Пауза";
    toggle.addEventListener("click", () => void sessionCommand("pause", session.monitor));
  }
  const remove = document.createElement("button");
  remove.type = "button";
  remove.textContent = "Убрать";
  remove.title = "Вернуть обычные обои Windows";
  remove.addEventListener("click", () => void sessionCommand("stop", session.monitor));
  controls.append(toggle, remove);
  row.append(controls);
  return row;
}

async function sessionCommand(command: "pause" | "resume" | "stop", monitor: number) {
  await call<string>(command, { monitor });
  await refreshSessions();
}

async function previewUrl(id: string): Promise<string | undefined> {
  const cached = previewCache.get(id);
  if (cached) return cached;
  try {
    const data = await invoke<string>("library_preview", { id });
    const url = `data:image/jpeg;base64,${data}`;
    previewCache.set(id, url);
    return url;
  } catch {
    return undefined;
  }
}

async function refreshLibrary() {
  libraryItems = await call<LibraryItem[]>("library_list");
  libraryBox.replaceChildren();
  const empty = libraryItems.length === 0;
  dropzone.classList.toggle("expanded", empty);
  if (empty) return;
  libraryItems.sort((a, b) => b.imported_at - a.imported_at);
  for (const item of libraryItems) {
    libraryBox.append(renderCard(item));
  }
  markActiveCards();
}

function markActiveCards() {
  const activePaths = new Set(
    activeSessions.map((session) => normalizePath(session.path ?? "")),
  );
  for (const card of libraryBox.querySelectorAll<HTMLElement>(".card")) {
    const file = card.dataset.file ?? "";
    card.classList.toggle("active", activePaths.has(normalizePath(file)));
  }
}

function renderCard(item: LibraryItem): HTMLElement {
  const card = document.createElement("div");
  card.className = "card";
  card.dataset.file = item.file;
  card.title = "Кликните, чтобы поставить на рабочий стол";

  const thumb = document.createElement("div");
  thumb.className = "thumb";
  if (item.preview) {
    const img = document.createElement("img");
    void previewUrl(item.id).then((url) => {
      if (url) img.src = url;
    });
    thumb.append(img);
  } else {
    thumb.textContent = item.kind === "video" ? "видео" : "картинка";
  }
  const overlay = document.createElement("div");
  overlay.className = "thumb-overlay";
  overlay.textContent = "Поставить";
  thumb.append(overlay);
  const badge = document.createElement("div");
  badge.className = "badge";
  badge.textContent = "на столе";
  thumb.append(badge);
  card.append(thumb);

  const footer = document.createElement("div");
  footer.className = "card-footer";
  const name = document.createElement("div");
  name.className = "name";
  name.textContent = item.name;
  name.title = item.name;
  footer.append(name);

  const menu = document.createElement("div");
  menu.className = "card-menu";
  const exportButton = document.createElement("button");
  exportButton.type = "button";
  exportButton.textContent = "⇪";
  exportButton.title = "Поделиться: сохранить как .wpk-файл";
  exportButton.addEventListener("click", (event) => {
    event.stopPropagation();
    void exportLibraryItem(item);
  });
  const removeButton = document.createElement("button");
  removeButton.type = "button";
  removeButton.textContent = "✕";
  removeButton.title = "Удалить из библиотеки";
  removeButton.addEventListener("click", (event) => {
    event.stopPropagation();
    void call<void>("library_remove", { id: item.id }).then(() => {
      previewCache.delete(item.id);
      void refreshLibrary();
    });
  });
  menu.append(exportButton, removeButton);
  footer.append(menu);
  card.append(footer);

  card.addEventListener("click", () => void applyLibraryItem(item));
  return card;
}

async function applyLibraryItem(item: LibraryItem) {
  report(`Ставлю «${item.name}»…`);
  const status = await call<string>("play", {
    path: item.file,
    monitor: selectedMonitor,
    quality: qualitySelect.value,
    volume: Number(volumeRange.value),
    anime4k: anime4kCheckbox.checked,
  });
  report(friendlyVerdict(status) ?? `«${item.name}» теперь на рабочем столе`);
  await refreshSessions();
}

/// The daemon acknowledgements are technical English; keep the useful part.
function friendlyVerdict(status: string): string | undefined {
  if (status.includes("Anime4K") && status.includes("active")) {
    return "Готово — аниме-улучшение включено";
  }
  if (status.includes("FSR active")) {
    return "Готово — улучшение картинки включено";
  }
  if (status.includes(">= monitor resolution")) {
    return "Готово. Улучшение не нужно: исходник не меньше экрана";
  }
  return undefined;
}

async function importPaths(paths: string[]) {
  importButton.disabled = true;
  try {
    for (const path of paths) {
      const gif = path.toLowerCase().endsWith(".gif");
      report(gif ? "Конвертирую GIF в видео…" : "Добавляю в библиотеку…");
      try {
        const item = await call<LibraryItem>("library_import", { path });
        report(`«${item.name}» добавлено в библиотеку`);
      } catch {
        // error text is already in the message line
      }
    }
  } finally {
    importButton.disabled = false;
  }
  await refreshLibrary();
}

async function importDialog() {
  const picked = await open({
    multiple: true,
    filters: [
      {
        name: "Видео, картинки и пакеты LimeWall",
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
          "wpk",
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

async function exportLibraryItem(item: LibraryItem) {
  const target = await save({
    defaultPath: `${item.name}.wpk`,
    filters: [{ name: "Пакет LimeWall", extensions: ["wpk"] }],
  });
  if (typeof target !== "string") return;
  await call<void>("library_export", { id: item.id, target });
  report(`«${item.name}» сохранено как ${target}`);
}

function volumeLabel(value: number): string {
  return value === 0 ? "выкл" : String(value);
}

window.addEventListener("DOMContentLoaded", () => {
  connectButton.addEventListener("click", () => void connect());
  importButton.addEventListener("click", () => void importDialog());
  void getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "over") {
      document.body.classList.add("dragging");
    } else {
      document.body.classList.remove("dragging");
    }
    if (event.payload.type === "drop") {
      void importPaths(event.payload.paths);
    }
  });

  volumeRange.addEventListener("input", () => {
    volumeValue.textContent = volumeLabel(Number(volumeRange.value));
  });
  volumeRange.addEventListener("change", () => {
    if (!connected || activeSessions.length === 0) return;
    void call<string>("set_volume", {
      monitor: selectedMonitor,
      volume: Number(volumeRange.value),
    }).catch(() => {});
  });

  const pushQuality = () => {
    if (!connected || activeSessions.length === 0) return;
    void call<string>("set_quality", {
      monitor: selectedMonitor,
      quality: qualitySelect.value,
      anime4k: anime4kCheckbox.checked,
    })
      .then((status) => report(friendlyVerdict(status) ?? status))
      .catch(() => {});
  };
  qualitySelect.addEventListener("change", pushQuality);
  anime4kCheckbox.addEventListener("change", pushQuality);

  autostartCheckbox.addEventListener("change", () => {
    void call<string>("set_autostart", { enabled: autostartCheckbox.checked })
      .then(() =>
        report(
          autostartCheckbox.checked
            ? "LimeWall будет запускаться вместе с Windows"
            : "Автозапуск выключен",
        ),
      )
      .catch(() => {
        autostartCheckbox.checked = !autostartCheckbox.checked;
      });
  });
  batterySelect.addEventListener("change", () => {
    void call<string>("set_battery_policy", { policy: batterySelect.value })
      .then(() => report("Настройка батареи сохранена"))
      .catch(() => {});
  });

  void connect();
});
