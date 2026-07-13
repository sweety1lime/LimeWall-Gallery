# Песочница рендерера — исследование (Windows)

Статус: **research/design, код не писан**. Задача — честно оценить, можно ли
изолировать процесс `renderer`, который 24/7 проигрывает потенциально
недоверенный контент, и предложить реализуемый путь. Конкурентов изучаем, код не
копируем (правило проекта).

## Что процесс делает сегодня (факты из кода)

Любая изоляция должна пережить это:

- **Парентинг за иконки рабочего стола**: `SetParent(наше_окно, WorkerW)`, где
  WorkerW — окно, принадлежащее **explorer.exe** (Medium IL).
  См. `crates/platform/src/win32.rs:1066-1072`, discovery — `:796-844`.
- **Видео/картинки через libmpv**: `libmpv-2.dll` грузится в рантайме через
  `libloading` (crate `mpv`); DLL **не подписана Microsoft**; аппаратное
  декодирование через D3D11VA, окно отдаётся по `--wid`.
- **Web/3D через WebView2** (`wry`): `crates/platform/src/win32.rs:934`, с нашим
  `WALLPAPER_CSP` (без выхода в сеть) и запретом новых окон (`:950`). WebView2
  крутит вложенный message-pump и **сам** запускает контент в многопроцессной
  Chromium-песочнице.
- **Дочерние процессы**: демон запускает UI по «Open LimeWall» (`daemon.rs:1533`),
  CLI-подкоманды зовут ffmpeg (`main.rs:537`).
- **Файлы**: библиотека в `%APPDATA%/LimeWall`, IPC — локальный сокет/named pipe.

## Модель угроз

Что реально защищаем: **эксплойт бага парсинга в декодере** (C-код libmpv/ffmpeg
разбирает недоверенное видео → RCE), либо побег из WebView2/JS.

Что уже снижает риск (не надо переоценивать угрозу для беты):
- Галерея — **только медиа**, модерация PR, проверка **SHA-256**, **отзыв**
  (kill-switch). Код-обои (web/3D) требуют **явного согласия** и живут под CSP.
- WebView2 **уже** изолирует веб-контент своей песочницей; наш CSP убирает сеть.
- Значит **самая ценная цель песочницы — нативный путь libmpv/ffmpeg**, а не web.

## Варианты изоляции и конфликты

### A. Полный AppContainer на весь `renderer` — **отклонено**

AppContainer — это уровень ниже Low IL. Перепарентить наше окно **под окно
explorer.exe (Medium IL)** через `SetParent` — это модификация чужого окна
процессом с более низким IL, что блокирует **UIPI** (User Interface Privilege
Isolation). То есть `SetParent(WorkerW)` из AppContainer **почти наверняка
провалится** (нужно подтвердить спайком, но приоритет высокий).

То же касается запуска всего процесса на **Low IL** — WorkerW-парентинг ломается
по той же причине. Вывод: **нельзя изолировать процесс, который сам лезет в
WorkerW.** Привилегированную часть нужно отделить.

### B. Разделение: привилегированный host + песочница-декодер — **целевой путь**

По образцу изоляции в браузерах:

- **Host** (Medium IL, минимальный): владеет WS_CHILD-окном под WorkerW, делает
  `SetParent`, композитит кадр на поверхность. Ничего не парсит.
- **Декодер** (сильно ограничен: AppContainer / restricted token): гоняет libmpv
  через **render API** (`mpv_render_context_create`, OpenGL/D3D-callback), рисует
  в **общую GPU-текстуру** (D3D11 shared handle + keyed mutex), которую host
  выводит на обои. Декодер **не касается окон explorer** → UIPI не мешает, и его
  можно душить по-настоящему: AppContainer + ACG (`DYNAMIC_CODE_PROHIBITED`, у
  ffmpeg/libmpv нет CPU-JIT) + запрет дочерних процессов + строгий токен.
- **IPC** host↔декодер: наш существующий локальный сокет (ACL с SID AppContainer)
  или анонимный pipe.

Цена: заметная сложность (по-мониторный процесс-декодер, плумбинг shared-surface,
оверхед кросс-процессного композита), плюс переезд на libmpv render API (мы и так
знаем про его нужность на macOS, где `--wid` не работает). Это работа «когда
оправдано» — перед приёмом UGC или ростом, не для закрытой беты.

### C. Web/3D — полагаемся на песочницу WebView2

WebView2 уже запускает контент в отдельных Chromium-процессах (browser/renderer/
GPU — каждый в своём AppContainer). Наша задача — **не ослаблять** её (никаких
`--no-sandbox`, оставить CSP без сети, запрет новых окон уже есть). Отдельно
песочницу для web строить не нужно; риск здесь сравнительно закрыт.

### D. Process-mitigation policies на текущий процесс — **реализуемо сейчас**

Не меняя архитектуру, можно навесить Win32-митигации (при спавне через
`UpdateProcThreadAttribute` + `PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY`, либо в
рантайме `SetProcessMitigationPolicy`). Совместимость с нашим стеком:

| Митигация | Совместимо? | Причина |
|---|---|---|
| DEP/NX, ASLR (bottom-up + high-entropy), SEHOP | ✅ | дефолт на x64, без побочек |
| CFG (Control Flow Guard) | ✅ | собрать наш код с `-C control-flow-guard`; libmpv/WebView2 уже с CFG |
| Extension Point Disable | ✅ | режет инъекцию legacy hook/AppInit-DLL |
| Prefer System32 / no-remote / no-low-IL образы | ✅ | ужесточает загрузку DLL |
| CIG — только MS-подписанные DLL | ❌ | libmpv-2.dll, ffmpeg, WebView2-loader не MS-подписаны |
| ACG — запрет динамического кода | ❌ | ломает JIT WebView2 (Chromium) |
| Win32k syscall disable | ❌ | нужен GDI/USER для окон |
| Запрет дочерних процессов | ❌ | демон запускает UI, CLI зовёт ffmpeg |

Итог D: набор «бесплатных» митигаций снижает эксплуатируемость **без** ломки
WorkerW/WebView2. Эффект скромный, но реальный и малорисковый. Хороший **Фаза 1**.

## Что делают конкуренты (изучено, код не копировали)

- **Lively** (GPL): обои — отдельные дочерние процессы (mpv / web-рендер),
  перепарентятся под рабочий стол; сильной песочницы нет; web — через WebView2.
- **Wallpaper Engine** (проприетарная): фон + web/плагины через CEF (Chromium-
  песочница). Исторически были инциденты с воркшоп-контентом — подтверждает, что
  риск недоверенного контента реален, и что Chromium-песочница берёт часть на себя.

Вывод из обзора: сильной готовой модели «песочница движка обоев» у конкурентов
нет; изоляцию нативного декодера каждый решает слабо, а web закрывает Chromium.

## Рекомендация

1. **Фаза 1 (сейчас, низкий риск) — ✅ сделано.** Совместимые process-mitigation
   policies (extension-point-disable + image-load: no-remote / no-low-label /
   prefer-system32) применяются в `platform::harden_process()`
   (`crates/platform/src/harden_win32.rs`), вызывается первым в `renderer` main;
   CFG включён для Windows-MSVC сборок через `.cargo/config.toml`.
2. **Web/3D** — оставить как есть: песочница WebView2 + CSP; ничего не ослаблять.
3. **Фаза 2 (перед UGC / когда оправдано)** — вынести декодер в
   AppContainer-child через libmpv render API + shared-texture; host держит
   WorkerW. Оформить отдельным дизайном перед реализацией.
4. **Полный AppContainer на весь процесс — не делаем** (UIPI vs WorkerW).

## Проверить спайком перед кодом Фазы 2

- Реально ли `SetParent(WorkerW)` падает из AppContainer/Low IL (подтвердить UIPI).
- Живёт ли host-of-WebView2 при ограничениях; требования WebView2 к AppContainer.
- Перф shared-texture композита по каждому монитору vs текущий `--wid`.

## Ссылки

- Process mitigation policies — `SetProcessMitigationPolicy`, `UpdateProcThreadAttribute`
  (`PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY`), Microsoft Learn (win32/api/processthreadsapi).
- AppContainer isolation — `CreateAppContainerProfile`, Learn: «AppContainer Isolation».
- UIPI / уровни целостности — Learn: «Mandatory Integrity Control», `ChangeWindowMessageFilterEx`.
- Control Flow Guard — Learn: «Control Flow Guard»; Rust: `rustc` exploit-mitigations, `-C control-flow-guard`.
- WebView2 — Learn: microsoft-edge/webview2 (процессная модель и песочница).
- libmpv render API — `libmpv/render.h`, `mpv_render_context_create` (github.com/mpv-player/mpv).
