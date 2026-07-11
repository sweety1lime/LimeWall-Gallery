# Third-party компоненты и лицензии

Правило проекта: только LGPL (динамически) / MIT / Apache-2.0 / BSD.
Каждый бинарник и файл фиксируется здесь с источником и хэшем.

## libmpv (LGPLv2.1+)

- **Что**: `libmpv-2.dll`, client API v2.5; ffmpeg (LGPL-конфигурация) статически влинкован внутрь dll.
- **Источник**: <https://github.com/zhongfly/mpv-winbuild> (GitHub Actions сборки mpv c `-Dgpl=false`),
  релиз **2026-07-10-e5486b96d7**, артефакт `mpv-dev-lgpl-x86_64-20260710-git-e5486b96d7.7z`.
- **SHA-256 архива**: `826F2F7FA72E8DF4912327703D9EF3CF7D6E5A0F42D8002A11A554142BED0616`.
- **Как получить**: `scripts/fetch-libmpv.ps1` (проверяет хэш, кладёт dll рядом с renderer.exe).
  В репозиторий dll не коммитится (`third_party/` в .gitignore).
- **Связывание**: только динамическое, загрузка в рантайме через `libloading`
  (crate `mpv`, собственные FFI-объявления — заголовки mpv не копировались).
- **TODO до публичного релиза**: собственная CI-сборка mpv с `-Dgpl=false` и LGPL-конфигом
  ffmpeg — сторонняя сборка удобна для разработки, но лицензионную гарантию даёт только своя.

## ffmpeg (LGPLv3)

- **Что**: `ffmpeg.exe` для импорт-пайплайна библиотеки (GIF → mp4, jpg-превью).
  Запускается отдельным процессом — в наш код ничего не линкуется.
- **Источник**: <https://github.com/zhongfly/mpv-winbuild>, релиз **2026-07-10-e5486b96d7**,
  артефакт `ffmpeg-lgpl-x86_64-git-35f8f4bdc.7z` (сборка с `--enable-version3`, без GPL-компонентов).
- **SHA-256 архива**: `4EBCF42AF804FC5B6119C1C2D248B2509707A773A3A1F76B81F97E77BE353E48`.
- **Как получить**: `scripts/fetch-ffmpeg.ps1`; в репозиторий бинарник не коммитится.
- **Важно про энкодеры**: libx264 в LGPL-сборке отсутствует (он GPL). GIF → mp4 кодируется
  `h264_mf` (Windows MediaFoundation, есть на любой Windows 10+), фолбэк — `libvpx-vp9` (BSD).
  Решение зафиксировано в docs/research/ffmpeg-import.md.

## FSR.glsl (MIT)

- **Что**: порт AMD FidelityFX Super Resolution 1.0.2 (EASU + RCAS) для mpv `glsl-shaders`.
- **Файл**: `assets/shaders/FSR.glsl` (закоммичен; MIT-заголовок AMD в шапке файла).
- **Источник**: gist agyild <https://gist.github.com/agyild/82219c545228d70c5604f865ce0b0ce5>.
- **Лицензия**: MIT (Copyright (c) 2021 Advanced Micro Devices, Inc.), текст в шапке файла.

## Anime4K GLSL v4.0.1 (MIT / public domain)

- **Что**: рекомендованная авторами цепочка **Mode A** для mpv (видимый апскейл;
  Mode B оказался слишком мягким); шесть файлов в `assets/shaders/anime4k/`.
- **Источник**: официальный релиз <https://github.com/bloc97/Anime4K/releases/tag/v4.0.1>,
  архив `Anime4K_v4.0.zip`.
- **SHA-256 архива**: `139CD282086457C5ADC79CAF7B75B8B825091D71C9B54958C18745FEA62D7ED7`.
- **Лицензия**: MIT для `Clamp_Highlights`, `Restore_CNN_M` и двух `Upscale_CNN`;
  public domain для двух `AutoDownscalePre`. Полные тексты лицензий сохранены в
  шапке каждого файла.
- **Профиль**: opt-in флаг `--anime4k`; шейдеры включаются только при апскейле и
  заменяют FSR, если одновременно выбран `--quality max`.

## three.js r160 (MIT) — вендоренный вьювер 3D

- **Что**: `three.module.min.js` + `examples/jsm` `GLTFLoader.js` и
  `BufferGeometryUtils.js` в `assets/web/viewer/`; используются генерируемым
  `viewer.html` для `type: model3d` (glTF/glb-обои).
- **Источник**: npm-пакет `three@0.160.0` (unpkg CDN), файлы `build/` и
  `examples/jsm/`.
- **Лицензия**: MIT (Copyright 2010-2023 Three.js Authors) — заголовок
  `@license` в `three.module.min.js`; jsm-файлы покрыты общей MIT-лицензией
  проекта three.js.
- **Замечание**: обои неинтерактивны (WorkerW-дети не получают ввод), поэтому
  OrbitControls не вендорим — модель авто-вращается в render-loop.

## Rust-зависимости (проверено при добавлении)

| Crate | Лицензия | Назначение |
|---|---|---|
| windows | MIT OR Apache-2.0 | Win32 API (crates/platform) |
| clap | MIT OR Apache-2.0 | CLI |
| anyhow / thiserror | MIT OR Apache-2.0 | ошибки |
| ctrlc | MIT OR Apache-2.0 | Ctrl+C teardown |
| libloading | ISC | загрузка libmpv-2.dll в рантайме |
| serde 1.0.228 / serde_json 1.0.150 | MIT OR Apache-2.0 | JSON-протокол IPC |
| interprocess 2.4.2 | 0BSD OR Apache-2.0 | локальные сокеты: Windows named pipes / Unix sockets |
| wry 0.55 / webview2-com 0.38 | MIT OR Apache-2.0 | web-обои (WebView2 позади иконок) |
| raw-window-handle 0.6 | MIT OR Apache-2.0 OR Zlib | HWND-хэндл для wry |
| tauri 2 / tauri-build 2 | MIT OR Apache-2.0 | каркас UI (apps/ui) |
| tauri-plugin-dialog 2 / tauri-plugin-opener 2 | MIT OR Apache-2.0 | нативный файловый диалог; открытие ссылок |
| dirs 6 | MIT OR Apache-2.0 | путь к %APPDATA% (библиотека) |
| sha2 0.10 | MIT OR Apache-2.0 | content-id элементов библиотеки |
| base64 0.22 | MIT OR Apache-2.0 | превью через invoke |
| zip 8 | MIT | контейнер .wpk (crates/wpk) |
| windows-registry 0.5 | MIT OR Apache-2.0 | ассоциация .wpk в HKCU |
| tauri-plugin-single-instance 2 | MIT OR Apache-2.0 | одно окно панели; второй запуск отдаёт argv первому |
| winresource 0.1 (build) | MIT | иконка в renderer.exe |
| tempfile 3 (dev) | MIT OR Apache-2.0 | временные каталоги в тестах |

## Frontend-зависимости UI (npm, apps/ui)

| Пакет | Лицензия | Назначение |
|---|---|---|
| @tauri-apps/api, @tauri-apps/plugin-dialog, @tauri-apps/plugin-opener | MIT OR Apache-2.0 | invoke-мост и плагины |
| vite | MIT | сборка фронтенда |
| typescript | Apache-2.0 | типизация |

Транзитивные зависимости нового IPC-стека проверены по Cargo manifests:
0BSD / MIT / Apache-2.0 / Unlicense; `unicode-ident` дополнительно содержит
permissive Unicode-3.0 для таблиц идентификаторов. Copyleft-зависимостей нет.

Отклонено: crate `libmpv2` (и `libmpv-rs`) — LGPL-2.1; статическая линковка Rust-кода
навесила бы LGPL-обязательства на бинарник (см. docs/research/libmpv.md).
