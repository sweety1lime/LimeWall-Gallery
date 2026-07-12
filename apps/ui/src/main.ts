import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ask, open, save } from "@tauri-apps/plugin-dialog";

// ---------------------------------------------------------------------------
// types (mirror the Tauri backend; see src-tauri/src/lib.rs & crates/ipc)
// ---------------------------------------------------------------------------

type Quality = "eco" | "balanced" | "max";
// `model3d` is forward-compatible: the backend currently reports 3D packages
// as `web`, so the 3D tab stays empty until the daemon distinguishes them.
type Kind = "video" | "image" | "web" | "model3d";

interface Monitor {
  id: number;
  name: string;
  bounds: { x: number; y: number; width: number; height: number };
  scale: number;
  is_primary: boolean;
}

type PausedReason = "user" | "resources" | "game" | "battery" | "lock" | "display_off";

interface SessionStatus {
  monitor: number;
  state: "playing" | "paused" | "stopped";
  path: string | null;
  quality: Quality;
  volume: number;
  anime4k: boolean;
  paused_reason?: PausedReason | null;
}

interface LibraryItem {
  id: string;
  name: string;
  kind: Kind;
  file: string;
  preview: string | null;
  imported_at: number;
  author?: string | null;
  license?: string | null;
}

interface PackageInfo {
  name: string;
  kind: Kind;
}

interface PlaylistSummary {
  monitor: number;
  len: number;
  interval_minutes: number;
  shuffle: boolean;
}

interface GalleryPack {
  id: string;
  name: string;
  author: string;
  type: string;
  license: string;
  sha256: string;
  size: number;
  preview?: string | null;
  download_url: string;
  tags: string[];
}

interface DaemonStatus {
  sessions: SessionStatus[];
  stack_cpu_percent: number | null;
  playlists: PlaylistSummary[];
}

interface DiagCheck {
  id: string;
  status: string; // pass | fail | warn | info
  detail: string;
}

interface DiagReport {
  checks: DiagCheck[];
  log_tail: string;
  ui_version: string;
}

// Emitted by the backend when a double-clicked package runs code and needs
// explicit consent before install.
interface ConsentRequest {
  path: string;
  name: string;
  kind: string;
}

// ---------------------------------------------------------------------------
// tiny hyperscript helper — keeps the vanilla DOM building readable
// ---------------------------------------------------------------------------

type Child = Node | string | null | undefined | false;
// `html` sets innerHTML (inline SVG); `on*` keys attach listeners; everything
// else becomes an attribute.
type Attrs = Record<string, string | EventListener | undefined>;

function h(tag: string, attrs: Attrs = {}, ...children: Child[]): HTMLElement {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value == null) continue;
    if (key === "class") node.className = value as string;
    else if (key === "html") node.innerHTML = value as string;
    else if (key.startsWith("on") && typeof value === "function") {
      node.addEventListener(key.slice(2).toLowerCase(), value as EventListener);
    } else node.setAttribute(key, String(value));
  }
  for (const child of children) {
    if (child == null || child === false) continue;
    node.append(child);
  }
  return node;
}

// ---------------------------------------------------------------------------
// inline SVG icons
// ---------------------------------------------------------------------------

const icons = {
  logo: `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="#0e1610" stroke-width="2.4" stroke-linecap="round"><path d="M3 8c3-3 6 3 9 0s6-3 9 0M3 16c3-3 6 3 9 0s6-3 9 0"/></svg>`,
  gear: `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9"><circle cx="12" cy="12" r="3.2"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M18.4 5.6l-2.1 2.1M7.7 16.3l-2.1 2.1"/></svg>`,
  min: `<svg width="13" height="13" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2"><path d="M5 12h14"/></svg>`,
  max: `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="4" y="4" width="16" height="16" rx="2"/></svg>`,
  close: `<svg width="13" height="13" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 6l12 12M18 6L6 18"/></svg>`,
  power: `<svg width="30" height="30" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M18.4 5.6a9 9 0 11-12.8 0M12 3v9"/></svg>`,
  refresh: `<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><path d="M4 4v6h6M20 20v-6h-6M20 9a8 8 0 00-14-3M4 15a8 8 0 0014 3"/></svg>`,
  upload: `<svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M12 16V4M8 8l4-4 4 4M4 15v3a2 2 0 002 2h12a2 2 0 002-2v-3"/></svg>`,
  share: `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M12 15V4M8 8l4-4 4 4M4 15v3a2 2 0 002 2h12a2 2 0 002-2v-3"/></svg>`,
  shareSm: `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M12 15V4M8 8l4-4 4 4M4 15v3a2 2 0 002 2h12a2 2 0 002-2v-3"/></svg>`,
  trash: `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><path d="M4 7h16M9 7V5a1 1 0 011-1h4a1 1 0 011 1v2M6 7l1 13a1 1 0 001 1h8a1 1 0 001-1l1-13"/></svg>`,
  plus: `<svg width="15" height="15" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>`,
  play: `<svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor"><path d="M7 5l12 7-12 7z"/></svg>`,
  pause: `<svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor"><rect x="6" y="5" width="4" height="14" rx="1"/><rect x="14" y="5" width="4" height="14" rx="1"/></svg>`,
  speaker: `<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="#c2cad6" stroke-width="1.8" stroke-linecap="round"><path d="M4 9v6h3l5 4V5L7 9H4z" fill="#c2cad6" stroke="none"/><path d="M17 8l4 8M21 8l-4 8"/></svg>`,
  monitor: `<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><rect x="2" y="4" width="20" height="14" rx="2"/><path d="M8 21h8"/></svg>`,
  battery: `<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><rect x="4" y="8" width="14" height="10" rx="2"/><path d="M18 11h2v4h-2"/></svg>`,
  lock: `<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"><rect x="5" y="11" width="14" height="9" rx="2"/><path d="M8 11V8a4 4 0 018 0v3"/></svg>`,
};

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------

let connected = false;
let monitors: Monitor[] = [];
let selectedMonitor = 0;
let libraryItems: LibraryItem[] = [];
let activeSessions: SessionStatus[] = [];
let stackCpu: number | null = null;
let playlists: PlaylistSummary[] = [];
let playlistPopoverOpen = false;
let playlistInterval = Number(localStorage.getItem("lw-pl-interval")) || 15;
let playlistShuffle = localStorage.getItem("lw-pl-shuffle") === "1";
let libraryFilter: Kind | "all" = "all";
let settingsOpen = false;
let heroId: string | null = null;
let diagReport: DiagReport | null = null;
let diagRunning = false;
let galleryOpen = false;
let galleryPacks: GalleryPack[] = [];
let galleryLoading = false;
let galleryError: string | null = null;
const galleryDownloaded = new Set<string>(); // gallery pack ids, this session
// Library item ids that came from the gallery — persisted, drives the
// "✓ из каталога" badge on library cards.
const galleryVerified = new Set<string>(readJson<string[]>("lw-gallery-verified", []));

function readJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

function persistVerified() {
  localStorage.setItem("lw-gallery-verified", JSON.stringify([...galleryVerified]));
}

// Control values for the selected monitor: UI-owned, used at play time and
// pushed live to an active session. Adopted from a session on monitor switch.
let quality: Quality = "balanced";
let volume = 0;
let anime4k = false;
let autostart = false;
let autostartAvailable = false;
let battery = "pause";
let batteryAvailable = false;

let refreshTimer: number | undefined;
let toastTimer: number | undefined;
const previewCache = new Map<string, string>();

const content = document.getElementById("content") as HTMLElement;
const settingsOverlay = document.getElementById("settings") as HTMLElement;
const connPill = document.getElementById("conn") as HTMLElement;
const connTextEl = document.getElementById("conn-text") as HTMLElement;
const toastEl = document.getElementById("toast") as HTMLElement;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

function report(text: string, isError = false) {
  if (!text) {
    toastEl.hidden = true;
    return;
  }
  toastEl.textContent = text;
  toastEl.classList.toggle("error", isError);
  toastEl.hidden = false;
  if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (toastEl.hidden = true), isError ? 6000 : 3500);
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

const TYPE_META: Record<Kind, { label: string; cls: string }> = {
  video: { label: "Видео", cls: "video" },
  image: { label: "Картинка", cls: "image" },
  web: { label: "Веб", cls: "web" },
  model3d: { label: "3D", cls: "model3d" },
};

// Deterministic decorative gradient for items without a rendered preview.
const GRADIENTS = [
  "radial-gradient(140% 120% at 15% 8%, rgba(120,240,180,.55), transparent 55%), linear-gradient(140deg,#07231d,#0d3a30 45%,#1f6f57 75%,#7ff0b0)",
  "radial-gradient(120% 100% at 82% 0%, rgba(255,80,200,.5), transparent 60%), linear-gradient(160deg,#0a0f2a,#241047 40%,#7a1f6b 70%,#ff5db1)",
  "radial-gradient(120% 120% at 28% 100%, rgba(60,200,220,.45), transparent 55%), linear-gradient(200deg,#03141f,#062a3a 45%,#0e5d6e 80%,#39c6d8)",
  "radial-gradient(100% 100% at 70% 28%, rgba(200,120,255,.6), transparent 55%), linear-gradient(150deg,#0a0716,#1e1040 45%,#4a1f7a 75%,#c874ff)",
  "radial-gradient(120% 120% at 18% 92%, rgba(150,220,140,.42), transparent 55%), linear-gradient(180deg,#04140c,#0a2c1a 45%,#1c5232 80%,#6fae5e)",
  "radial-gradient(120% 120% at 50% 0%, rgba(255,120,60,.5), transparent 55%), linear-gradient(190deg,#1a0a08,#3a140d 45%,#7a2e18 78%,#ff7a3d)",
];

function gradientFor(id: string): string {
  let hash = 0;
  for (let i = 0; i < id.length; i++) hash = (hash * 31 + id.charCodeAt(i)) | 0;
  return GRADIENTS[Math.abs(hash) % GRADIENTS.length];
}

function sessionFor(monitor: number): SessionStatus | undefined {
  return activeSessions.find((s) => s.monitor === monitor && s.state !== "stopped");
}

function currentHeroItem(): LibraryItem | undefined {
  if (heroId) {
    const chosen = libraryItems.find((i) => i.id === heroId);
    if (chosen) return chosen;
  }
  const session = sessionFor(selectedMonitor);
  const fromSession = itemForPath(session?.path ?? null);
  if (fromSession) return fromSession;
  return libraryItems[0];
}

// Load a preview into a background element; falls back to the gradient.
function paintPreview(target: HTMLElement, item: LibraryItem) {
  if (item.preview) {
    void previewUrl(item.id).then((url) => {
      if (url) {
        target.style.backgroundImage = `url("${url}")`;
        target.classList.remove("gradient");
      }
    });
  }
}

// ---------------------------------------------------------------------------
// connection / lifecycle
// ---------------------------------------------------------------------------

function setConnected(version: string | null) {
  connected = version !== null;
  connPill.classList.toggle("offline", !connected);
  connTextEl.textContent = connected ? "подключено" : "нет связи";
  connTextEl.className = "conn-text";
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
  renderContent();
}

async function connect() {
  try {
    const version = await call<string>("daemon_connect");
    setConnected(version);
    report("");
    await refreshMonitors();
    await refreshLibrary();
    await refreshSessions();
    // Adopt the restored session's volume/quality/anime4k so the dock reflects
    // reality on first connect (not just after a monitor switch).
    adoptSessionControls(selectedMonitor);
    try {
      autostart = await invoke<boolean>("get_autostart");
      autostartAvailable = true;
    } catch {
      autostartAvailable = false;
    }
    try {
      battery = await invoke<string>("get_battery_policy");
      batteryAvailable = true;
    } catch {
      batteryAvailable = false;
    }
    renderContent();
    if (settingsOpen) renderSettings();
  } catch {
    setDisconnected();
    report("Не удалось запустить фоновый плеер. Нажмите «Подключить», чтобы повторить.", true);
  }
}

async function refreshMonitors() {
  monitors = await call<Monitor[]>("list_monitors");
  if (monitors.length && !monitors.some((m) => m.id === selectedMonitor)) {
    selectedMonitor = monitors[0].id;
  }
}

async function refreshLibrary() {
  libraryItems = await call<LibraryItem[]>("library_list");
  libraryItems.sort((a, b) => b.imported_at - a.imported_at);
  renderContent();
}

async function refreshSessions() {
  const status = await call<DaemonStatus>("daemon_status");
  activeSessions = status.sessions;
  stackCpu = status.stack_cpu_percent ?? null;
  playlists = status.playlists ?? [];
  // A poll only changes the play state and the "on desktop" markers — update
  // just those in place so the drift animation, hover and a volume drag survive.
  updateSessionUI();
}

let lastSelectedPath: string | null = null;

// Light in-place update for the 3s poll: politeness pill, play button, badges.
function updateSessionUI() {
  const polite = content.querySelector<HTMLElement>(".polite");
  if (polite) polite.replaceWith(buildPolite());

  const session = sessionFor(selectedMonitor);
  const isPlaying = session?.state === "playing";
  const playBtn = content.querySelector<HTMLElement>(".play-btn");
  if (playBtn) {
    playBtn.innerHTML = isPlaying ? icons.pause : icons.play;
    playBtn.title = isPlaying ? "Пауза" : "Играть";
  }
  markActiveCards();

  // Follow a playlist rotation: when the selected monitor's wallpaper changes
  // and the hero isn't pinned to a user pick, rebuild the hero to match.
  const path = session?.path ?? null;
  if (path !== lastSelectedPath) {
    lastSelectedPath = path;
    if (heroId === null) refreshHero();
  }
}

function markActiveCards() {
  const activePaths = new Set(
    activeSessions
      .filter((s) => s.state !== "stopped")
      .map((s) => normalizePath(s.path ?? "")),
  );
  for (const card of content.querySelectorAll<HTMLElement>(".card")) {
    const file = card.dataset.file ?? "";
    card.classList.toggle("active", activePaths.has(normalizePath(file)));
  }
}

// Adopt an active session's playback settings when switching to its monitor.
function adoptSessionControls(monitor: number) {
  const session = sessionFor(monitor);
  if (session) {
    quality = session.quality;
    volume = session.volume;
    anime4k = session.anime4k;
  }
}

// ---------------------------------------------------------------------------
// top-level view routing
// ---------------------------------------------------------------------------

function renderContent() {
  content.replaceChildren();
  if (!connected) {
    content.append(buildOffline());
  } else if (galleryOpen) {
    content.append(buildGallery());
  } else if (libraryItems.length === 0) {
    content.append(buildEmpty());
  } else {
    content.append(buildPopulated());
    markActiveCards();
  }
}

// ---------------------------------------------------------------------------
// gallery (community catalog)
// ---------------------------------------------------------------------------

function openGallery() {
  if (!connected) return;
  galleryOpen = true;
  // Show the last-seen catalog instantly, then refresh in the background.
  if (galleryPacks.length === 0) galleryPacks = readJson<GalleryPack[]>("lw-catalog-cache", []);
  renderContent();
  void loadGallery();
}

async function loadGallery() {
  galleryLoading = true;
  galleryError = null;
  renderContent();
  try {
    galleryPacks = await invoke<GalleryPack[]>("gallery_fetch_catalog");
    localStorage.setItem("lw-catalog-cache", JSON.stringify(galleryPacks));
  } catch {
    // Keep the cached catalog if we have one; only hard-fail when empty.
    if (galleryPacks.length === 0) {
      galleryError = "Не удалось загрузить каталог. Проверьте интернет и попробуйте ещё раз.";
    } else {
      report("Нет сети — показан сохранённый каталог");
    }
  } finally {
    galleryLoading = false;
    renderContent();
  }
}

function buildGallery(): HTMLElement {
  const head = h(
    "div",
    { class: "gallery-head" },
    h(
      "button",
      { class: "btn-ghost small", type: "button", onClick: () => { galleryOpen = false; renderContent(); } },
      "← Библиотека",
    ),
    h("div", { class: "gallery-title" }, "Каталог обоев"),
    h(
      "button",
      { class: "btn-ghost small", type: "button", onClick: () => void loadGallery() },
      "Обновить",
    ),
  );

  const body = h("div", { class: "grid-scroll" });
  if (galleryLoading) {
    body.append(h("div", { class: "grid-empty" }, "Загружаю каталог…"));
  } else if (galleryError) {
    body.append(h("div", { class: "grid-empty" }, galleryError));
  } else if (galleryPacks.length === 0) {
    body.append(
      h(
        "div",
        { class: "grid-empty" },
        "Каталог пока пуст. Загляните позже — или предложите свои обои через GitHub.",
      ),
    );
  } else {
    const grid = h("div", { class: "grid" });
    for (const pack of galleryPacks) grid.append(renderGalleryCard(pack));
    body.append(grid);
  }

  return h("div", { class: "gallery" }, head, body);
}

function renderGalleryCard(pack: GalleryPack): HTMLElement {
  const kind = (["video", "image", "web", "model3d"].includes(pack.type) ? pack.type : "video") as Kind;
  const type = TYPE_META[kind];

  const thumb = h(
    "div",
    { class: "card-thumb gradient" },
    h("span", { class: "card-type type-badge " + type.cls }, type.label),
  );
  thumb.style.backgroundImage = gradientFor(pack.id);
  if (pack.preview) {
    const img = new Image();
    const url = pack.preview;
    img.onload = () => {
      thumb.style.backgroundImage = `url("${url}")`;
      thumb.classList.remove("gradient");
    };
    img.src = url;
  }

  const done = galleryDownloaded.has(pack.id);
  const dlBtn = h(
    "button",
    {
      class: "gallery-dl" + (done ? " done" : ""),
      type: "button",
      onClick: (e: Event) => {
        e.stopPropagation();
        if (!done) void downloadPack(pack);
      },
    },
    done ? "✓ В библиотеке" : "Скачать",
  );

  const foot = h(
    "div",
    { class: "card-foot" },
    h(
      "div",
      { class: "card-foot-info" },
      h("div", { class: "card-name", title: pack.name }, pack.name),
      h(
        "div",
        { class: "card-author", title: `Лицензия: ${pack.license}` },
        `${pack.author} · ${formatSize(pack.size)}`,
      ),
    ),
    dlBtn,
  );

  return h("div", { class: "card gallery-card" }, thumb, foot);
}

async function downloadPack(pack: GalleryPack) {
  report(`Скачиваю «${pack.name}»…`);
  try {
    const item = await call<LibraryItem>("gallery_download", { pack });
    galleryDownloaded.add(pack.id);
    galleryVerified.add(item.id);
    persistVerified();
    report(`«${item.name}» добавлено в библиотеку`);
    // refreshLibrary → renderContent; galleryOpen is still true so it redraws
    // the gallery with this card now marked as downloaded.
    await refreshLibrary();
  } catch {
    // error text is already shown by call()
  }
}

// ---------------------------------------------------------------------------
// offline view
// ---------------------------------------------------------------------------

function buildOffline(): HTMLElement {
  return h(
    "div",
    { class: "state-center" },
    h("div", { class: "state-badge warn", html: icons.power }),
    h(
      "div",
      { class: "col-gap" },
      h("div", { class: "state-title" }, "Фоновый плеер не запущен"),
      h(
        "div",
        { class: "state-text" },
        "Обои рисует отдельный фоновый процесс — он работает даже когда это окно закрыто. Сейчас связь с ним потеряна.",
      ),
    ),
    primaryButton("Подключить", icons.refresh, () => void connect()),
  );
}

// ---------------------------------------------------------------------------
// empty view (drag & drop)
// ---------------------------------------------------------------------------

function buildEmpty(): HTMLElement {
  const formats = [
    h("span", { class: "format-chip" }, "MP4 · MKV · WEBM"),
    h("span", { class: "format-chip" }, "GIF"),
    h("span", { class: "format-chip" }, "PNG · JPG"),
    h("span", { class: "format-chip accent" }, "HTML · 3D · WPK"),
  ];
  return h(
    "div",
    { class: "empty-wrap" },
    h(
      "div",
      { class: "dropzone" },
      h("div", { class: "state-badge lime", html: icons.upload }),
      h(
        "div",
        { class: "col-gap center" },
        h("div", { class: "state-title" }, "Перетащите файл сюда"),
        h(
          "div",
          { class: "state-text" },
          "Видео, GIF, картинка, HTML-страница или 3D-модель. GIF автоматически станет видео. Готовые обои — файлом .wpk.",
        ),
      ),
      h("div", { class: "format-chips" }, ...formats),
      h(
        "div",
        { class: "empty-actions" },
        primaryButton("Выбрать файлы", icons.plus, () => void importDialog()),
        h(
          "button",
          { class: "btn-ghost", type: "button", onClick: () => void importSample() },
          iconSpan(icons.play),
          "Начать с примера",
        ),
        h(
          "button",
          { class: "btn-ghost", type: "button", onClick: () => openGallery() },
          "Открыть каталог",
        ),
      ),
    ),
  );
}

// Imports the bundled first-party sample so a fresh library is not empty.
async function importSample() {
  report("Добавляю пример обоев…");
  try {
    const item = await call<LibraryItem>("import_bundled_sample");
    report(`«${item.name}» добавлено в библиотеку`);
  } catch {
    // error text is already shown by call()
  }
  await refreshLibrary();
}

// ---------------------------------------------------------------------------
// populated view: hero + library
// ---------------------------------------------------------------------------

function buildPopulated(): HTMLElement {
  return h("div", { class: "populated" }, buildHero(), buildLibrary());
}

function buildHero(): HTMLElement {
  const item = currentHeroItem();
  const bg = h("div", { class: "hero-bg gradient" });
  if (item) {
    bg.style.backgroundImage = gradientFor(item.id);
    paintPreview(bg, item);
  }

  const hero = h(
    "div",
    { class: "hero", "data-hero": "1" },
    bg,
    h("div", { class: "hero-scrim" }),
    buildHeroTop(),
    buildHeroBottom(item),
  );
  return hero;
}

function refreshHero() {
  const existing = content.querySelector<HTMLElement>('[data-hero="1"]');
  if (existing) existing.replaceWith(buildHero());
}

function buildHeroTop(): HTMLElement {
  const monSwitch = h("div", { class: "monitor-switch" });
  if (monitors.length >= 2) {
    for (const monitor of monitors) {
      const chip = h(
        "button",
        {
          class: "mon-chip" + (monitor.id === selectedMonitor ? " active" : ""),
          type: "button",
          title: monitor.name + (monitor.is_primary ? " (основной)" : ""),
          onClick: () => {
            selectedMonitor = monitor.id;
            adoptSessionControls(monitor.id);
            renderContent();
          },
        },
        "Монитор " + (monitor.id + 1),
      );
      monSwitch.append(chip);
    }
  }

  return h("div", { class: "hero-top" }, monSwitch, buildPolite());
}

function buildPolite(): HTMLElement {
  const session = sessionFor(selectedMonitor);
  let cls = "idle";
  let title = "Простаивает";
  let sub = "обои не запущены";
  if (session?.state === "playing") {
    cls = "";
    title = "Играет";
    sub = stackCpu != null ? `на рабочем столе · ${Math.round(stackCpu)}% CPU` : "на рабочем столе";
  } else if (session?.state === "paused") {
    const reason = session.paused_reason;
    if (reason === "battery") {
      cls = "battery";
      title = "Эконом";
      sub = "питание от батареи";
    } else {
      cls = "paused";
      title = "На паузе";
      sub =
        reason === "game"
          ? "запущена игра"
          : reason === "resources"
            ? "высокая нагрузка — приостановлено"
            : reason === "lock"
              ? "экран заблокирован"
              : reason === "display_off"
                ? "дисплей выключен"
                : reason === "user"
                  ? "поставлено вручную"
                  : "фоновый плеер приостановлен";
    }
  }

  return h(
    "div",
    { class: "polite " + cls },
    h("span", { class: "polite-dot" }),
    h(
      "div",
      { class: "polite-labels" },
      h("span", { class: "polite-title" }, title),
      h("span", { class: "polite-sub" }, sub),
    ),
    h("div", { class: "polite-bars" }, h("span"), h("span"), h("span")),
  );
}

function buildHeroBottom(item: LibraryItem | undefined): HTMLElement {
  const name = item?.name ?? "Нет обоев";
  const meta = h("div", { class: "hero-meta" });
  if (item) {
    const type = TYPE_META[item.kind];
    meta.append(h("span", { class: "type-badge " + type.cls }, type.label));
    if (item.author) {
      meta.append(h("span", { class: "sep" }, "·"), h("span", {}, item.author));
    }
  }

  const shareBtn = h(
    "button",
    {
      class: "btn-ghost",
      type: "button",
      onClick: () => item && void exportLibraryItem(item),
    },
    iconSpan(icons.share),
    "Поделиться .wpk",
  );

  const headrow = h(
    "div",
    { class: "hero-headrow" },
    h("div", {}, h("div", { class: "hero-name" }, name), meta),
    item ? shareBtn : null,
  );

  return h("div", { class: "hero-bottom" }, headrow, buildPlaylistStrip(), buildDock(item));
}

function buildPlaylistStrip(): HTMLElement | null {
  const pl = playlists.find((p) => p.monitor === selectedMonitor);
  if (!pl) return null;
  return h(
    "div",
    { class: "playlist-strip" },
    iconSpan(icons.play),
    h(
      "span",
      { class: "pl-info" },
      `Плейлист · ${pl.len} обоев · каждые ${pl.interval_minutes} мин${pl.shuffle ? " · перемешивание" : ""}`,
    ),
    h(
      "button",
      { class: "pl-btn", type: "button", onClick: () => void playlistNextCmd() },
      "Следующие",
    ),
    h(
      "button",
      { class: "pl-btn danger", type: "button", onClick: () => void stopPlaylist() },
      "✕ Остановить",
    ),
  );
}

function buildDock(item: LibraryItem | undefined): HTMLElement {
  const session = sessionFor(selectedMonitor);
  const isPlaying = session?.state === "playing";

  const playBtn = h(
    "button",
    {
      class: "play-btn",
      type: "button",
      title: isPlaying ? "Пауза" : "Играть",
      html: isPlaying ? icons.pause : icons.play,
      onClick: () => void togglePlay(item),
    },
  );

  // quality segmented control
  const qSeg = h("div", { class: "seg" });
  const qDefs: [Quality, string][] = [
    ["eco", "Эконом"],
    ["balanced", "Баланс"],
    ["max", "Максимум"],
  ];
  for (const [key, label] of qDefs) {
    qSeg.append(
      h(
        "button",
        {
          class: "seg-btn" + (quality === key ? " active" : ""),
          type: "button",
          onClick: () => setQuality(key),
        },
        label,
      ),
    );
  }

  const monLabel = "Качество · монитор " + (selectedMonitor + 1);
  const qualityBlock = h(
    "div",
    { class: "dock-block" },
    h("span", { class: "dock-label" }, monLabel),
    qSeg,
  );

  // volume
  const slider = h("input", {
    type: "range",
    min: "0",
    max: "100",
    value: String(volume),
  }) as HTMLInputElement;
  slider.addEventListener("input", () => {
    volume = Number(slider.value);
  });
  slider.addEventListener("change", () => pushVolume());
  const audioBlock = h(
    "div",
    { class: "dock-block audio" },
    h("span", { class: "dock-label" }, "Звук"),
    h("div", { class: "audio-row" }, iconSpan(icons.speaker), slider),
  );

  // anime4k
  const animeBlock = h(
    "div",
    { class: "dock-block" },
    h("span", { class: "dock-label" }, "Anime4K"),
    h(
      "div",
      {
        class: "toggle",
        onClick: () => toggleAnime4k(),
      },
      h("span", { class: "switch" + (anime4k ? " on" : "") }),
      h("span", { class: "toggle-text" }, anime4k ? "вкл" : "выкл"),
    ),
  );

  return h("div", { class: "dock" }, playBtn, qualityBlock, audioBlock, animeBlock);
}

function buildLibrary(): HTMLElement {
  const tabs = h("div", { class: "tabs" });
  const tabDefs: [Kind | "all", string][] = [
    ["all", "Все"],
    ["video", "Видео"],
    ["image", "Картинки"],
    ["web", "Веб"],
    ["model3d", "3D"],
  ];
  for (const [key, label] of tabDefs) {
    const count =
      key === "all" ? libraryItems.length : libraryItems.filter((i) => i.kind === key).length;
    tabs.append(
      h(
        "button",
        {
          class: "tab" + (libraryFilter === key ? " active" : ""),
          type: "button",
          onClick: () => {
            libraryFilter = key;
            renderContent();
          },
        },
        label,
        h("span", { class: "count" }, String(count)),
      ),
    );
  }

  const shown = libraryItems.filter(
    (i) => libraryFilter === "all" || i.kind === libraryFilter,
  );

  const headActions = h("div", { class: "head-actions" });
  if (shown.length >= 2) {
    headActions.append(
      h(
        "button",
        {
          class: "btn-ghost small" + (playlistPopoverOpen ? " active" : ""),
          type: "button",
          onClick: () => {
            playlistPopoverOpen = !playlistPopoverOpen;
            renderContent();
          },
        },
        iconSpan(icons.play),
        "Играть все",
      ),
    );
  }
  headActions.append(primarySmall("Добавить", icons.plus, () => void importDialog()));

  const head = h("div", { class: "library-head" }, tabs, headActions);

  const grid = h("div", { class: "grid" });
  if (shown.length === 0) {
    grid.append(h("div", { class: "grid-empty" }, "В этой категории пока пусто."));
  } else {
    for (const item of shown) grid.append(renderCard(item));
  }

  return h(
    "div",
    { class: "library" },
    head,
    playlistPopoverOpen && shown.length >= 2 ? buildPlaylistPopover(shown) : null,
    h("div", { class: "grid-scroll" }, grid),
  );
}

function buildPlaylistPopover(items: LibraryItem[]): HTMLElement {
  const seg = h("div", { class: "seg" });
  for (const minutes of [1, 5, 15, 30, 60]) {
    seg.append(
      h(
        "button",
        {
          class: "seg-btn" + (playlistInterval === minutes ? " active" : ""),
          type: "button",
          onClick: () => {
            playlistInterval = minutes;
            localStorage.setItem("lw-pl-interval", String(minutes));
            renderContent();
          },
        },
        `${minutes} мин`,
      ),
    );
  }

  const shuffleToggle = h(
    "div",
    {
      class: "toggle",
      onClick: () => {
        playlistShuffle = !playlistShuffle;
        localStorage.setItem("lw-pl-shuffle", playlistShuffle ? "1" : "0");
        renderContent();
      },
    },
    h("span", { class: "switch" + (playlistShuffle ? " on" : "") }),
    h("span", { class: "toggle-text" }, "Перемешивать"),
  );

  return h(
    "div",
    { class: "playlist-popover" },
    h(
      "div",
      { class: "pl-row" },
      h("span", { class: "dock-label" }, "Смена каждые"),
      seg,
    ),
    h("div", { class: "pl-row" }, shuffleToggle),
    primarySmall(
      `Запустить ${items.length} на мониторе ${selectedMonitor + 1}`,
      icons.play,
      () => void startPlaylist(items),
    ),
  );
}

async function startPlaylist(items: LibraryItem[]) {
  playlistPopoverOpen = false;
  // Let the hero follow the rotation instead of a pinned pick.
  heroId = null;
  report("Запускаю плейлист…");
  try {
    await call<string>("set_playlist", {
      monitor: selectedMonitor,
      items: items.map((i) => i.file),
      intervalMinutes: playlistInterval,
      shuffle: playlistShuffle,
      quality,
      volume,
      anime4k,
    });
    report(`Плейлист из ${items.length} обоев запущен`);
  } catch {
    // error text is already shown by call()
  }
  await refreshSessions();
  renderContent();
}

async function playlistNextCmd() {
  await call<string>("playlist_next", { monitor: selectedMonitor }).catch(() => {});
  await refreshSessions();
  refreshHero();
}

async function stopPlaylist() {
  await call<string>("clear_playlist", { monitor: selectedMonitor }).catch(() => {});
  await refreshSessions();
  renderContent();
}

function renderCard(item: LibraryItem): HTMLElement {
  const type = TYPE_META[item.kind];

  const thumb = h(
    "div",
    { class: "card-thumb gradient" },
    h("span", { class: "card-type type-badge " + type.cls }, type.label),
    h("span", { class: "card-active-badge" }, "✓ На столе"),
    h("span", { class: "card-active-ring" }),
  );
  thumb.style.backgroundImage = gradientFor(item.id);
  paintPreview(thumb, item);

  const shareBtn = h(
    "button",
    {
      class: "card-act share",
      type: "button",
      title: "Поделиться (.wpk)",
      html: icons.shareSm,
      onClick: (e: Event) => {
        e.stopPropagation();
        void exportLibraryItem(item);
      },
    },
  );
  const delBtn = h(
    "button",
    {
      class: "card-act del",
      type: "button",
      title: "Удалить",
      html: icons.trash,
      onClick: (e: Event) => {
        e.stopPropagation();
        void removeLibraryItem(item);
      },
    },
  );

  const foot = h(
    "div",
    { class: "card-foot" },
    h(
      "div",
      { class: "card-foot-info" },
      h("div", { class: "card-name", title: item.name }, item.name),
      galleryVerified.has(item.id)
        ? h(
            "div",
            { class: "card-author verified", title: "Скачано из каталога LimeWall" },
            `✓ из каталога · ${item.author ?? "LimeWall"}`,
          )
        : h("div", { class: "card-author" }, item.author ?? "локально"),
    ),
    h("div", { class: "card-actions" }, shareBtn, delBtn),
  );

  const card = h("div", { class: "card", onClick: () => void applyLibraryItem(item) }, thumb, foot);
  card.dataset.file = item.file;
  card.dataset.id = item.id;
  return card;
}

// ---------------------------------------------------------------------------
// settings slide-over
// ---------------------------------------------------------------------------

function openSettings() {
  settingsOpen = true;
  renderSettings();
}

function closeSettings() {
  settingsOpen = false;
  settingsOverlay.hidden = true;
  settingsOverlay.replaceChildren();
}

function renderSettings() {
  settingsOverlay.hidden = false;
  settingsOverlay.replaceChildren(
    h("div", { class: "settings-scrim", onClick: () => closeSettings() }),
    buildSettingsPanel(),
  );
}

function buildSettingsPanel(): HTMLElement {
  const header = h(
    "div",
    { class: "settings-header" },
    h("span", { class: "settings-title" }, "Настройки"),
    h("button", { class: "icon-btn", type: "button", html: icons.close, onClick: () => closeSettings() }),
  );

  // system section: autostart + battery
  const autoSwitch = h("div", { class: "switch big" + (autostart ? " on" : "") });
  const autoCard = h(
    "div",
    { class: "setting-card" + (autostartAvailable ? "" : " disabled") },
    h(
      "div",
      { class: "setting-row" },
      h(
        "div",
        {},
        h("div", { class: "setting-name" }, "Запускать с Windows"),
        h("div", { class: "setting-desc" }, "обои появятся сразу после входа"),
      ),
      h("div", { class: "toggle", onClick: () => toggleAutostart() }, autoSwitch),
    ),
  );

  const batteryCard = h(
    "div",
    { class: "setting-card" + (batteryAvailable ? "" : " disabled") },
    h(
      "div",
      {},
      h("div", { class: "setting-name" }, "Питание от батареи"),
      h("div", { class: "setting-desc" }, "что делать на ноутбуке без розетки"),
    ),
    buildSegWide(
      "blue",
      [
        ["pause", "Пауза"],
        ["eco", "Эконом"],
        ["keep", "Держать"],
      ],
      battery,
      (v) => setBattery(v),
    ),
    h(
      "span",
      { class: "setting-hint" },
      {
        pause: "Обои встают на паузу, пока ноутбук на батарее.",
        eco: "Временно снижает качество и отключает шейдеры, возврат при подключении к сети.",
        keep: "Ничего не меняется — играет как обычно.",
      }[battery] ?? "",
    ),
  );

  const systemSection = h(
    "div",
    { class: "settings-section" },
    h("span", { class: "section-label" }, "Система"),
    autoCard,
    batteryCard,
  );

  // default quality section
  const animeSwitch = h("div", { class: "switch big" + (anime4k ? " on" : "") });
  const qualityCard = h(
    "div",
    { class: "setting-card" },
    buildSegWide(
      "lime",
      [
        ["eco", "Эконом"],
        ["balanced", "Баланс"],
        ["max", "Максимум"],
      ],
      quality,
      (v) => setQuality(v as Quality),
    ),
    h(
      "span",
      { class: "setting-hint" },
      {
        eco: "Bilinear без шейдеров — самая низкая нагрузка.",
        balanced: "Lanczos — чёткая картинка при малой нагрузке.",
        max: "FSR-апскейл на GPU — максимальная резкость для контента ниже разрешения экрана.",
      }[quality],
    ),
    h("div", { class: "setting-divider" }),
    h(
      "div",
      { class: "setting-row" },
      h(
        "div",
        {},
        h("div", { class: "setting-name" }, "Anime4K"),
        h("div", { class: "setting-desc" }, "резкость для рисованного контента"),
      ),
      h("div", { class: "toggle", onClick: () => toggleAnime4k() }, animeSwitch),
    ),
  );

  const qualitySection = h(
    "div",
    { class: "settings-section" },
    h("span", { class: "section-label" }, "Качество по умолчанию"),
    qualityCard,
  );

  // auto-pause info
  const autoPauseSection = h(
    "div",
    { class: "settings-section" },
    h("span", { class: "section-label" }, "Автопауза (экономия)"),
    h(
      "div",
      { class: "setting-list" },
      settingListRow(icons.monitor, "Полноэкранная игра или видео", "≈ 0% CPU"),
      settingListRow(icons.battery, "Разряд батареи", "по политике выше"),
      settingListRow(icons.lock, "Экран заблокирован или выключен", "→ пауза"),
    ),
  );

  return h(
    "div",
    { class: "settings-panel" },
    header,
    systemSection,
    qualitySection,
    autoPauseSection,
    buildDiagnosticsSection(),
  );
}

const DIAG_LABELS: Record<string, string> = {
  daemon: "Фоновый плеер",
  renderer_exe: "Файл renderer.exe",
  libmpv: "Библиотека libmpv-2.dll",
  ffmpeg: "Конвертер ffmpeg",
  monitors: "Мониторы",
  desktop_icons: "Иконки рабочего стола",
  sessions: "Активные обои",
  autostart: "Автозапуск",
  daemon_log: "Журнал плеера",
};

const DIAG_MARK: Record<string, string> = { pass: "✓", fail: "✕", warn: "⚠", info: "•" };

function buildDiagnosticsSection(): HTMLElement {
  const rows = h("div", { class: "setting-list" });
  if (diagReport) {
    for (const c of diagReport.checks) {
      rows.append(
        h(
          "div",
          { class: "setting-list-row diag-row " + c.status },
          h("span", { class: "diag-mark" }, DIAG_MARK[c.status] ?? "•"),
          h(
            "div",
            { class: "diag-body" },
            h("div", { class: "diag-name" }, DIAG_LABELS[c.id] ?? c.id),
            h("div", { class: "diag-detail" }, c.detail),
          ),
        ),
      );
    }
  } else {
    rows.append(h("div", { class: "setting-hint" }, "Проверка ещё не запускалась."));
  }

  const runBtn = h(
    "button",
    { class: "btn-primary small", type: "button", onClick: () => void runDiagnostics() },
    diagRunning ? "Проверяю…" : "Проверить",
  );
  const actions = h("div", { class: "diag-actions" }, runBtn);
  if (diagReport) {
    actions.append(
      h(
        "button",
        { class: "btn-ghost", type: "button", onClick: () => void copyDiagReport() },
        "Скопировать отчёт",
      ),
    );
  }

  return h(
    "div",
    { class: "settings-section" },
    h("span", { class: "section-label" }, "Диагностика"),
    actions,
    rows,
  );
}

async function runDiagnostics() {
  diagRunning = true;
  if (settingsOpen) renderSettings();
  try {
    diagReport = await call<DiagReport>("run_diagnostics");
  } finally {
    diagRunning = false;
    if (settingsOpen) renderSettings();
  }
}

async function copyDiagReport() {
  if (!diagReport) return;
  const lines = [
    `LimeWall диагностика · UI ${diagReport.ui_version}`,
    "",
    ...diagReport.checks.map(
      (c) => `[${c.status.toUpperCase()}] ${DIAG_LABELS[c.id] ?? c.id}: ${c.detail}`,
    ),
    "",
    "--- журнал плеера ---",
    diagReport.log_tail || "(пусто)",
  ];
  const text = lines.join("\n");
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Fallback for environments without the async clipboard API.
    const area = document.createElement("textarea");
    area.value = text;
    document.body.append(area);
    area.select();
    document.execCommand("copy");
    area.remove();
  }
  report("Отчёт скопирован в буфер обмена");
}

function buildSegWide(
  tone: "lime" | "blue",
  defs: [string, string][],
  active: string,
  onPick: (v: string) => void,
): HTMLElement {
  const seg = h("div", { class: "seg-wide " + tone });
  for (const [key, label] of defs) {
    seg.append(
      h(
        "button",
        {
          class: active === key ? "active" : "",
          type: "button",
          onClick: () => onPick(key),
        },
        label,
      ),
    );
  }
  return seg;
}

function settingListRow(icon: string, label: string, right: string): HTMLElement {
  return h(
    "div",
    { class: "setting-list-row" },
    h("span", { class: "lime-ic", html: icon }),
    h("span", {}, label),
    h("span", { class: "badge-right" }, right),
  );
}

// ---------------------------------------------------------------------------
// small builders
// ---------------------------------------------------------------------------

function iconSpan(svg: string): HTMLElement {
  return h("span", { class: "ic", html: svg });
}

function primaryButton(label: string, icon: string, onClick: () => void): HTMLElement {
  return h("button", { class: "btn-primary", type: "button", onClick }, iconSpan(icon), label);
}

function primarySmall(label: string, icon: string, onClick: () => void): HTMLElement {
  return h("button", { class: "btn-primary small", type: "button", onClick }, iconSpan(icon), label);
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

async function togglePlay(item: LibraryItem | undefined) {
  const session = sessionFor(selectedMonitor);
  if (session?.state === "playing") {
    await call<string>("pause", { monitor: selectedMonitor });
  } else if (session?.state === "paused") {
    await call<string>("resume", { monitor: selectedMonitor });
  } else if (item) {
    await applyLibraryItem(item);
    return;
  } else {
    return;
  }
  await refreshSessions();
}

async function applyLibraryItem(item: LibraryItem) {
  heroId = item.id;
  report(`Ставлю «${item.name}»…`);
  const status = await call<string>("play", {
    path: item.file,
    monitor: selectedMonitor,
    quality,
    volume,
    anime4k,
  });
  report(friendlyVerdict(status) ?? `«${item.name}» теперь на рабочем столе`);
  await refreshSessions();
  refreshHero();
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

function setQuality(next: Quality) {
  quality = next;
  refreshHero();
  if (settingsOpen) renderSettings();
  if (!connected || !sessionFor(selectedMonitor)) return;
  void call<string>("set_quality", { monitor: selectedMonitor, quality, anime4k })
    .then((status) => report(friendlyVerdict(status) ?? status))
    .catch(() => {});
}

function toggleAnime4k() {
  anime4k = !anime4k;
  refreshHero();
  if (settingsOpen) renderSettings();
  if (!connected || !sessionFor(selectedMonitor)) return;
  void call<string>("set_quality", { monitor: selectedMonitor, quality, anime4k })
    .then((status) => report(friendlyVerdict(status) ?? status))
    .catch(() => {});
}

function pushVolume() {
  if (!connected || !sessionFor(selectedMonitor)) return;
  void call<string>("set_volume", { monitor: selectedMonitor, volume }).catch(() => {});
}

async function removeLibraryItem(item: LibraryItem) {
  await call<void>("library_remove", { id: item.id });
  previewCache.delete(item.id);
  if (heroId === item.id) heroId = null;
  if (galleryVerified.delete(item.id)) persistVerified();
  await refreshLibrary();
}

// Human-readable file size.
function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) return (bytes / 1024 / 1024 / 1024).toFixed(1) + " ГБ";
  if (bytes >= 1024 * 1024) return Math.round(bytes / 1024 / 1024) + " МБ";
  if (bytes >= 1024) return Math.round(bytes / 1024) + " КБ";
  return bytes + " Б";
}

function toggleAutostart() {
  if (!autostartAvailable) return;
  const next = !autostart;
  autostart = next;
  renderSettings();
  void call<string>("set_autostart", { enabled: next })
    .then(() =>
      report(next ? "LimeWall будет запускаться вместе с Windows" : "Автозапуск выключен"),
    )
    .catch(() => {
      autostart = !next;
      renderSettings();
    });
}

function setBattery(policy: string) {
  if (!batteryAvailable) return;
  battery = policy;
  renderSettings();
  void call<string>("set_battery_policy", { policy })
    .then(() => report("Настройка батареи сохранена"))
    .catch(() => {});
}

// ---------------------------------------------------------------------------
// import / export
// ---------------------------------------------------------------------------

// A web / 3D wallpaper ships code that runs on the desktop; ask before adding.
async function confirmCodeWallpaper(name: string, kind: string): Promise<boolean> {
  const label = kind === "model3d" ? "3D" : "веб";
  return await ask(
    `«${name}» — интерактивные ${label}-обои: они запускают собственный код на вашем рабочем столе.\n\n` +
      "Устанавливайте такие обои только из доверённых источников. Выход в интернет для них заблокирован LimeWall.\n\n" +
      "Добавить в библиотеку?",
    { title: "Обои с исполняемым кодом", kind: "warning", okLabel: "Добавить", cancelLabel: "Отмена" },
  );
}

async function importPaths(paths: string[]) {
  for (const path of paths) {
    const lower = path.toLowerCase();
    const gif = lower.endsWith(".gif");
    const isHtml = lower.endsWith(".html") || lower.endsWith(".htm");
    const isWpk = lower.endsWith(".wpk");

    // Code-bearing imports (a bare HTML web wallpaper, or a web/3D .wpk) run JS
    // on the desktop — get explicit consent before installing them. Plain media
    // is inert and imports without friction.
    let needsConsent = isHtml;
    let displayName = path.split(/[\\/]/).pop() ?? path;
    let kind = "web";
    if (isWpk) {
      try {
        const info = await invoke<PackageInfo>("inspect_package", { path });
        displayName = info.name;
        kind = info.kind;
        needsConsent = info.kind === "web" || info.kind === "model3d";
      } catch {
        needsConsent = true; // can't classify → ask rather than trust
      }
    }
    if (needsConsent && !(await confirmCodeWallpaper(displayName, kind))) {
      report("Импорт отменён");
      continue;
    }

    report(gif ? "Конвертирую GIF в видео…" : isHtml ? "Добавляю веб-обои…" : "Добавляю в библиотеку…");
    try {
      const item = await call<LibraryItem>("library_import", { path });
      report(`«${item.name}» добавлено в библиотеку`);
    } catch {
      // error text is already shown by call()
    }
  }
  await refreshLibrary();
}

async function importDialog() {
  const picked = await open({
    multiple: true,
    filters: [
      {
        name: "Видео, картинки, веб и пакеты LimeWall",
        extensions: [
          "mp4", "mkv", "webm", "mov", "avi", "m4v",
          "gif", "png", "jpg", "jpeg", "bmp", "webp",
          "html", "htm", "wpk",
        ],
      },
    ],
  });
  if (Array.isArray(picked)) await importPaths(picked);
  else if (typeof picked === "string") await importPaths([picked]);
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

// ---------------------------------------------------------------------------
// window chrome
// ---------------------------------------------------------------------------

function wireTitlebar() {
  document.getElementById("tb-logo")!.innerHTML = icons.logo;
  document.getElementById("settings-open")!.innerHTML = icons.gear;
  const min = document.getElementById("win-min")!;
  const max = document.getElementById("win-max")!;
  const close = document.getElementById("win-close")!;
  min.innerHTML = icons.min;
  max.innerHTML = icons.max;
  close.innerHTML = icons.close;
  const win = getCurrentWindow();
  min.addEventListener("click", () => void win.minimize());
  max.addEventListener("click", () => void win.toggleMaximize());
  close.addEventListener("click", () => void win.close());
  document.getElementById("settings-open")!.addEventListener("click", () => openSettings());
  document.getElementById("gallery-open")!.addEventListener("click", () => openGallery());
}

// ---------------------------------------------------------------------------
// boot
// ---------------------------------------------------------------------------

window.addEventListener("DOMContentLoaded", () => {
  wireTitlebar();

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && settingsOpen) closeSettings();
  });

  // A second app launch (double-clicked .wpk) imports in the backend and
  // pings us to show the result.
  void listen("library-changed", () => void refreshLibrary());
  // Code-bearing packages are not imported silently — the backend hands them
  // here for explicit consent first.
  void listen<ConsentRequest>("wpk-consent", async (event) => {
    const { path, name, kind } = event.payload;
    if (!(await confirmCodeWallpaper(name, kind))) {
      report("Импорт отменён");
      return;
    }
    report(`Добавляю «${name}»…`);
    try {
      const item = await call<LibraryItem>("library_import", { path });
      report(`«${item.name}» добавлено в библиотеку`);
    } catch {
      // error text is already shown by call()
    }
    await refreshLibrary();
  });
  void getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "over") document.body.classList.add("dragging");
    else document.body.classList.remove("dragging");
    if (event.payload.type === "drop") void importPaths(event.payload.paths);
  });

  void connect();
});
