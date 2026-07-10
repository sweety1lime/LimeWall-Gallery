# LiveWall

Cross-platform live wallpaper engine (working title). Windows → Linux → macOS.
Renders video / GIF (→ mp4) / images behind the desktop icons via libmpv, with GPU-side
upscaling, at minimal system cost.

- **Architecture:** two processes — a long-lived `renderer` daemon (wallpaper
  surfaces + libmpv playback, controlled over local IPC) and a closable UI.
  Platform specifics stay behind the `WallpaperHost` trait in `crates/platform`.
- **Roadmap & acceptance criteria:** see [PLAN.md](PLAN.md)
- **Research notes & decisions:** see [docs/](docs/)

## Status

Phase 0 — *"colored layer behind the icons"* — implemented and verified on
Windows 10 22H2 (WorkerW research: [docs/research/workerw.md](docs/research/workerw.md)).
Pending: Windows 11 / 24H2 and multi-monitor checks.

Phase 1 — *"video and images behind the icons"* — implemented:

- `crates/mpv`: hand-written libmpv FFI (no LGPL crates), dll loaded at runtime
  ([docs/research/libmpv.md](docs/research/libmpv.md)).
- Pinned LGPL libmpv build: run `scripts/fetch-libmpv.ps1` once
  ([docs/third-party.md](docs/third-party.md)).
- Play anything behind the icons:
  `cargo run -p renderer -- play video.mp4 --monitor 0 --quality max`
  (stdin controls: `pause` / `resume` / `volume N` / `screenshot <path>` / `stop`).
- Anime profile (official Anime4K v4.0.1 Mode B Fast): add `--anime4k`.
  It is enabled only when the source is smaller than the monitor and replaces
  FSR if combined with `--quality max`.

Verified live: H.264/HEVC 1080p via d3d11va hardware decoding, `pause` → 0% CPU,
4K still image → 0% CPU, FSR upscaling auto-enabled for 720p sources and
auto-skipped at native resolution.

Phase 2 (in progress) — renderer daemon with per-monitor sessions over local
IPC (named pipe / unix socket):

```
renderer serve
renderer ctl play video.mp4 --monitor 0 --quality max
renderer ctl pause | resume | volume 30 | quality eco | stop | status | shutdown
```

Control UI (`apps/ui`, Tauri 2 + vanilla TS): finds or starts the daemon,
lists monitors, applies wallpapers with quality/volume controls. Run with
`cd apps/ui && npm install && npm run tauri dev`. Anime4K Mode B Fast also passes an end-to-end
shader-loading smoke test on Windows 10 22H2.

Phase 2 foundation has started with a standalone `crates/ipc`: versioned and
validated JSON commands/responses with bounded length-prefixed framing. The
renderer daemon and Tauri client are the next slices; existing CLI playback is
still unchanged.
