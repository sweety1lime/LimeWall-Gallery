# Phase 2 architecture proposal — UI, IPC, library and import

Status: approved on 2026-07-10. Slices 2.1a (protocol + transport), 2.1b
(daemon skeleton) and 2.1c (per-monitor playback sessions: play/stop/pause/
resume/volume/quality, verified live) are done. The named-pipe timeout
limitation is addressed as planned: each connection runs on its own thread and
forwards decoded requests over a channel to the daemon thread, so a stalled
client blocks only its own connection thread (16 concurrent connections max).
The Tauri UI shell (rest of task 2.1) is done: `apps/ui` finds the daemon or
spawns it detached and drives it through tauri commands; verified by an
end-to-end test that spawns the real renderer (a live window run is still
pending a free desktop). Next: the library (2.2). Phase 1 remains open only
for unavailable hardware checks (physical second monitor and final
720p → 1440p/4K comparison).

## 1. Process ownership

The existing two-process architecture stays unchanged:

- `renderer` is the long-lived daemon. It owns wallpaper surfaces, libmpv
  players, persisted state, the IPC server and (task 2.5) the tray icon.
- `ui` is a short-lived Tauri 2 client. Closing its window terminates the UI
  process; renderer and wallpapers continue.

The tray must belong to renderer, not Tauri. Otherwise the phase acceptance
criterion “UI process exited, tray still works” cannot be satisfied. “Open UI”
from the tray starts or focuses the separate UI executable.

At UI startup:

1. connect and send `ping`;
2. if no daemon answers, start renderer as an independent detached process;
3. retry the connection with a bounded timeout;
4. never tie renderer lifetime to the Tauri window/sidecar handle.

Tauri supports bundled external binaries, but its sidecar examples model child
processes controlled by the app. LimeWall needs the opposite lifetime, so the
Tauri Rust backend should resolve the bundled renderer path and spawn it
detached. Source: <https://v2.tauri.app/develop/sidecar/>.

## 2. UI stack

Recommended baseline:

- Tauri 2;
- Vanilla TypeScript + Vite;
- npm (`npm.cmd` on this Windows setup; Node 24.15.0, npm 11.12.1);
- no frontend framework until the library UI proves it needs one.

This is the smallest dependency/runtime surface and matches the current Tauri
recommendation to start with its Vanilla TypeScript template. Sources:
<https://v2.tauri.app/start/create-project/> and
<https://v2.tauri.app/start/frontend/vite/>.

## 3. IPC transport

Recommended crate: blocking `interprocess` local sockets, without its Tokio
feature. It maps to Windows named pipes now and Unix-domain local sockets on
Linux/macOS later, bypassing the network stack. License: 0BSD OR Apache-2.0.
Sources: <https://docs.rs/interprocess/latest/interprocess/> and
<https://github.com/kotauskas/interprocess>.

Why blocking I/O:

- one UI client and very small control messages do not need an async runtime;
- a listener blocked in the OS consumes approximately no CPU;
- fewer dependencies and simpler shutdown behavior;
- connection threads can forward decoded commands through `std::sync::mpsc`
  to the daemon thread that owns platform and mpv state.

Endpoint name v1: a namespaced local-socket name derived from `limewall`, the
protocol major version and the current user identity. Binding the endpoint is
also the single-daemon-instance lock. Exact Windows ACL/user-identity handling
must be verified before accepting external commands.

## 4. Protocol v1

Wire format: UTF-8 JSON with a 4-byte little-endian length prefix and a hard
1 MiB frame limit. Length framing avoids delimiter ambiguity while retaining
human-readable payloads.

Request envelope:

```json
{
  "version": 1,
  "id": 42,
  "command": {
    "type": "play",
    "monitor": 0,
    "path": "C:\\Wallpapers\\clip.mp4",
    "quality": "balanced",
    "volume": 0,
    "anime4k": false
  }
}
```

Response envelope:

```json
{
  "version": 1,
  "id": 42,
  "result": { "status": "playing" }
}
```

Errors use a stable code plus a human-readable message. Every response echoes
the request id. Unknown protocol versions, oversized frames, unknown commands,
relative media paths and invalid volume/monitor values are rejected.

Initial commands:

- `ping`;
- `list_monitors`;
- `status`;
- `play` per monitor;
- `stop` per monitor / all;
- `pause` and `resume` per monitor / all;
- `set_volume` per monitor;
- `set_quality` per monitor (restarts/reconfigures playback if required);
- `shutdown` (tray “Exit” only; not exposed directly to untrusted frontend JS).

The shared `crates/ipc` crate contains only protocol types, validation, framing,
blocking client/server transport and tests. It must not depend on Tauri, mpv or
the platform backend.

## 5. Renderer refactor

Current `play` is a monolithic CLI path. Refactor without breaking it:

1. extract mpv option construction, shader selection and loaded-media metadata;
2. introduce a `PlaybackSession` owning `mpv::Player`, `SurfaceHandle`, source
   path and per-monitor settings;
3. introduce a daemon manager owning one `WallpaperHost` and a
   `HashMap<MonitorId, PlaybackSession>`;
4. keep the existing CLI commands as adapters over the same session logic;
5. run IPC accept/connection work outside the daemon owner thread and forward
   commands through channels.

This preserves the rule that platform code stays in `crates/platform`, keeps
libmpv calls serialized by the daemon owner, and enables multiple monitors.

## 6. Implementation slices

### 2.1a — Protocol foundation

- add `crates/ipc` to the workspace;
- add `serde`, `serde_json`, `interprocess` after recording licenses;
- implement versioned request/response enums, validation and bounded framing;
- round-trip, malformed-frame, oversized-frame and version tests.

Acceptance: `cargo test --workspace --all-targets` and Clippy are clean; an IPC
in-memory/frame test covers every command variant.

Implementation result (2026-07-10): `crates/ipc` contains protocol v1 command,
response, monitor and session types; validation rejects incompatible versions,
invalid volume and relative/empty media paths; 4-byte little-endian JSON frames
are bounded to 1 MiB. Nine IPC tests cover all command variants, response JSON
shape, sequential frames, malformed/empty frames and oversized reads/writes.

### 2.1b — Renderer daemon skeleton

- add `renderer serve`;
- single-instance endpoint + `ping`, `list_monitors`, `status`, `shutdown`;
- clean Ctrl+C/tray-exit teardown;
- retain existing `play` and `test-surface` behavior.

Acceptance: a second renderer cannot bind; a client round-trip works; idle CPU
remains approximately 0%.

### 2.1c — Stateful playback over IPC

- extract `PlaybackSession` and per-monitor manager;
- connect play/stop/pause/resume/volume/quality commands;
- define replacement semantics when `play` targets an occupied monitor;
- return structured state/errors.

Acceptance: a CLI test client controls a real wallpaper without stdin and can
disconnect while playback continues.

### 2.1d — Minimal Tauri client

- scaffold `apps/ui` with Vanilla TypeScript + Vite + npm;
- Rust backend connects/spawns detached renderer and exposes narrow Tauri
  commands; frontend JS never receives arbitrary shell permissions;
- show connection state and monitor list; basic play/stop/pause/volume/quality
  controls only.

Acceptance: close UI process, renderer continues; reopen UI and recover status.

### 2.2 — Library

- define versioned library/config JSON schema;
- use `%APPDATA%/LimeWall/library` through platform-appropriate data dirs;
- atomic metadata writes and collision-safe content ids;
- drag-and-drop and preview grid in UI.

### 2.3 — Import pipeline

- copy videos/images into the library;
- fetch/package a pinned LGPL ffmpeg build and record source/hash;
- GIF → H.264 mp4 (`yuv420p`, even dimensions, `+faststart`, CRF 18–20);
- generate deterministic preview JPEG and report import progress/errors.

### 2.4 — Persisted assignments

- persist per-monitor media, quality, volume and Anime4K selection;
- renderer restores assignments on daemon/autostart startup;
- handle missing media and changed monitor topology without a crash loop.

### 2.5 — Tray and autostart

- renderer-owned tray: pause all / open UI / exit;
- registry `Run` opt-in setting;
- installer bundles renderer, UI, shaders, libmpv and ffmpeg with documented
  licenses.

## 7. Decisions requested

Recommended choices to approve before implementation:

1. Vanilla TypeScript + Vite + npm for `apps/ui`.
2. Blocking `interprocess` local sockets, no Tokio.
3. Renderer owns daemon state and tray; Tauri is a disposable IPC client.
4. Start with slice 2.1a only, then verify before touching renderer playback.
