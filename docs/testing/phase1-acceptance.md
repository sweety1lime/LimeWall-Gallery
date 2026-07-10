# Phase 1 acceptance checklist

Run from the repository root on a visible Windows desktop. Fetch libmpv first
with `scripts/fetch-libmpv.ps1` and use media that may legally be tested and
screenshotted.

## Playback and idle behavior

1. Play H.264 and HEVC 1080p files with `--quality balanced`.
2. Confirm the layer is behind clickable desktop icons and muted by default.
3. Confirm `hardware decoding` reports D3D11VA and record CPU/GPU usage.
4. Send `pause`; after two seconds confirm renderer CPU is approximately 0%.
5. Repeat with a static 4K image and confirm idle CPU is approximately 0%.

## Upscaling evidence

1. Use the procedure in `docs/comparisons/README.md` with the same 720p frame.
2. Confirm `max` reports FSR enabled and is visibly sharper than `eco`.
3. Confirm native-or-larger content reports FSR/Anime4K skipped.
4. Run `--anime4k`, confirm all shaders load, and record GPU frame time/load.

## Display topology

Supported display modes can be listed without changing the system:

```text
cargo run -p platform --example display_mode -- list "\\.\DISPLAY1"
```

The optional `cycle` command tests a mode first, applies it temporarily, and
restores the original mode through a guard. It intentionally accepts at most 30
seconds, but still use it only on a local test machine:

```text
cargo run -p platform --example display_mode -- cycle "\\.\DISPLAY1" 1600 900 12
```

1. Keep a video playing and change the selected monitor's resolution.
2. Wait up to two seconds; confirm the surface moves/resizes and playback stays
   alive.
3. If available, disconnect and reconnect that monitor. The surface should hide
   while absent and return to the same display device when it reappears.
4. Connect a second monitor and confirm the selected monitor remains targeted.
5. Stop playback and confirm the desktop is fully restored.

## Phase boundary

Update `PLAN.md` and the relevant research notes with OS build, hardware and
results. Phase 2 starts only after every phase 1 acceptance item is either
verified or explicitly documented as a known issue.
