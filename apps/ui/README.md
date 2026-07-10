# LiveWall UI

Tauri 2 control panel for the renderer daemon: connects to it (or starts it
detached), lists monitors, applies wallpapers and drives playback per monitor.

```
npm install
npm run tauri dev
```

The renderer executable is looked up via `LIVEWALL_RENDERER`, next to the UI
executable, then in the workspace `target/` directory. Backend tests:
`cd src-tauri && cargo test`.
