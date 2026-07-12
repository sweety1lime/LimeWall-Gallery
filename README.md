# LimeWall

Cross-platform live wallpaper engine by 2fame. Windows first (Linux → macOS
later). Renders video / GIF (→ mp4) / images / HTML / glTF behind the desktop
icons at minimal system cost, with GPU-side upscaling.

Still in active development — not a released product yet.

- **Architecture:** two processes — a long-lived `renderer` daemon (wallpaper
  surfaces + libmpv/WebView2 playback, controlled over local IPC) and a
  closable Tauri UI. Platform specifics stay behind the `WallpaperHost` trait
  in `crates/platform`.
- **Roadmap & acceptance criteria:** [PLAN.md](PLAN.md)
- **Research notes & decisions:** [docs/](docs/)

## Features

- **Video, GIF and image wallpapers** via libmpv with hardware decoding
  (d3d11va); GIF is converted to mp4 on import.
- **Web (HTML) and 3D (glTF) wallpapers** via WebView2, with a bundled
  three.js viewer for models.
- **GPU upscaling** — mpv lanczos + FSR, or Anime4K for drawn content; enabled
  only when the source is smaller than the monitor.
- **Per-monitor control** of quality, volume and Anime4K, plus **playlists**
  (interval + shuffle) that keep rotating with the window closed.
- **Politeness / minimal load** — decoding pauses (≈0% CPU) behind fullscreen
  apps, on the lock screen, with the display off, and on battery (pause / eco /
  keep). A resource watchdog also pauses any wallpaper that runs the CPU hot.
- **Safety for untrusted wallpapers** — web pages get a strict CSP (no network
  egress, no remote code); installing code-bearing web/3D packs asks for
  consent. See [docs/research/security-model.md](docs/research/security-model.md).
- **`.wpk` packages** (zip + manifest) — export any library item, import by
  dialog, drag-and-drop or double-click.
- **Community catalog** — browse and install shared wallpapers from the in-app
  Catalog tab; downloads are checksum-verified. See
  [gallery/README.md](gallery/README.md) and
  [docs/research/workshop.md](docs/research/workshop.md).

## Build & run

One-time setup for playback (pinned LGPL builds; see
[docs/third-party.md](docs/third-party.md)):

```
scripts\fetch-libmpv.ps1
scripts\fetch-ffmpeg.ps1
```

CLI playback and the daemon:

```
cargo run -p renderer -- play video.mp4 --monitor 0 --quality max
cargo run -p renderer -- serve
cargo run -p renderer -- ctl play video.mp4 --monitor 0 --quality max
cargo run -p renderer -- ctl pause | resume | volume 30 | status | shutdown
```

Control UI (finds or starts the daemon):

```
cd apps/ui
npm install
npm run tauri dev
```

Notes: Windows PowerShell 5 has no `&&` — run commands one per line. The first
`tauri dev` compiles the whole Tauri stack and can take several minutes before
the window appears.

`tauri dev` serves from a dev server and can't exercise the real desktop
lifecycle (closing the window, reopening from the tray, autostart, reboot).
For that, build the portable folder — every binary side by side, standalone:

```
scripts\build-portable.ps1
```

The result lands in `dist/LimeWall/`; run `dist\LimeWall\LimeWall.exe`.

The daemon owns a tray icon (pause all / resume all / next wallpaper / open UI /
quit), shows load in its tooltip, persists applied wallpapers and restores them
on start; autostart with Windows is a checkbox in the UI (or
`renderer ctl autostart on|off`).

## Licensing

Core (player + local import) is free. mpv and ffmpeg are used as **LGPL**
builds, dynamically linked. Shaders (FSR / Anime4K) are MIT/BSD. Pinned sources
are recorded under [docs/](docs/).
