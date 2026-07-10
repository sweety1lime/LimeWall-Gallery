# Phase 1 quality comparisons

This directory stores screenshots made from the same frame and monitor for the
`eco`, `max` and optional Anime4K profiles. Use a real 1280x720 source and a
2560x1440 or 3840x2160 monitor for the phase 1 acceptance evidence.

The repository also contains a reproducible synthetic 1280x720 chart generated
by `scripts/generate-upscale-test-chart.ps1`. Preliminary 1080p results and
their limitations are recorded in `2026-07-10-win10-1080p.md`.

Run each profile from the repository root, pause on the same frame, then use the
runtime screenshot command:

```text
cargo run -p renderer -- play <video> --monitor 0 --quality eco
pause
screenshot docs/comparisons/eco.png
stop

cargo run -p renderer -- play <video> --monitor 0 --quality max
pause
screenshot docs/comparisons/max.png
stop

cargo run -p renderer -- play <video> --monitor 0 --quality balanced --anime4k
pause
screenshot docs/comparisons/anime4k.png
stop
```

Use the same source timestamp for every profile. Record the source resolution,
monitor resolution, GPU, renderer build (`debug` or `release`) and whether mpv
reported hardware decoding. Do not add copyrighted source media to the
repository; only commit comparison crops/screenshots when redistribution is
permitted.
