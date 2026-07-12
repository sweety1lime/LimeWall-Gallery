# Third-party notices — LimeWall

LimeWall (by 2fame) is distributed with the third-party components listed below.
This notice satisfies the attribution and source-availability obligations of
their licenses. A full inventory of source-only dependencies (Rust crates, npm
packages) with their permissive licenses is in `docs/third-party.md`.

The GNU license texts referenced here ship next to this file as
`lgpl-2.1.txt`, `lgpl-3.0.txt` and `gpl-3.0.txt` (fetch them with
`scripts/fetch-licenses.ps1`). The full MIT license text is included once at the
end of this file.

---

## mpv — `libmpv-2.dll`  (LGPL-2.1-or-later)

- **Use:** the wallpaper renderer plays video through libmpv. The DLL is loaded
  at **runtime** via `libloading`; LimeWall's own code declares its own FFI and
  does not link the mpv headers. This is the "shared library mechanism" form of
  use permitted by the LGPL.
- **Composition:** this build statically links FFmpeg configured as LGPL with
  `--enable-version3` (see below). Combining mpv (LGPL-2.1-or-later) with those
  LGPL-3.0 components means the distributed `libmpv-2.dll` is conveyed under
  **LGPL-3.0-or-later**; both license texts are included.
- **Exact build:** `zhongfly/mpv-winbuild` release `2026-07-10-e5486b96d7`,
  artifact `mpv-dev-lgpl-x86_64-20260710-git-e5486b96d7.7z`, built with
  `-Dgpl=false`. Reproduce with `scripts/fetch-libmpv.ps1` (SHA-256 pinned).
- **Upstream source:** <https://mpv.io> · <https://github.com/mpv-player/mpv>.
- **Replacing it (LGPL relinking right):** because libmpv is a standalone DLL
  loaded at runtime, you may substitute your own build of a client-API-compatible
  `libmpv-2.dll` in the install folder and LimeWall will use it — no relinking of
  LimeWall is required.

## FFmpeg — `ffmpeg.exe`  (LGPL-3.0)

- **Use:** the library import pipeline runs `ffmpeg.exe` as a **separate
  process** (GIF → mp4 conversion, JPEG previews). Nothing from FFmpeg is linked
  into LimeWall.
- **Exact build:** `zhongfly/mpv-winbuild` release `2026-07-10-e5486b96d7`,
  artifact `ffmpeg-lgpl-x86_64-git-35f8f4bdc.7z`, configured with
  `--enable-version3` and **without** any GPL components. Reproduce with
  `scripts/fetch-ffmpeg.ps1` (SHA-256 pinned).
- **Upstream source:** <https://ffmpeg.org> · <https://git.ffmpeg.org/ffmpeg>.
- **Replacing it:** swap `ffmpeg.exe` for your own LGPL FFmpeg build; the import
  pipeline invokes it by path.

## AMD FidelityFX Super Resolution 1.0 — `shaders/FSR.glsl`  (MIT)

Copyright (c) 2021 Advanced Micro Devices, Inc. The MIT header is retained in the
file. Ported for mpv `glsl-shaders` (EASU + RCAS).

## Anime4K v4.0.1 (GLSL) — `shaders/anime4k/*.glsl`  (MIT / public domain)

Copyright (c) 2019-2021 bloc97 and the Anime4K contributors. MIT for
`Clamp_Highlights`, `Restore_CNN_M` and the two `Upscale_CNN` passes; public
domain for the two `AutoDownscalePre` passes. Each file retains its own license
header. Upstream: <https://github.com/bloc97/Anime4K>.

## three.js r160 — `web/viewer/*`  (MIT)

Copyright © 2010-2024 three.js authors. The `@license` header is retained in
`three.module.min.js`; the bundled `GLTFLoader.js` and `BufferGeometryUtils.js`
are covered by the same MIT license. Upstream: <https://threejs.org>.

---

## MIT License

Applies to the FSR, Anime4K (MIT portions) and three.js components above, with
the respective copyright holders named in each section.

```
Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
