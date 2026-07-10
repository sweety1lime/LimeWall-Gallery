# Research 0.1 — WorkerW: слой позади иконок на Windows 10/11 (включая 24H2/25H2)

Дата: 2026-07-10. Статус: **стратегия утверждена владельцем 2026-07-10**, реализована в `crates/platform/src/win32.rs`.

## TL;DR

Классический WorkerW-хак жив, но на Windows 11 24H2+ иерархия окон изменилась:
WorkerW теперь создаётся **внутри Progman** (child), а не top-level сиблингом.
Рабочая стратегия — **двухпутевой поиск** (классический путь + 24H2-путь) с retry
и фолбэком на сам Progman, плюс watchdog на пересоздание WorkerW. Так делают
актуальные реализации (Lively 2.1+ переписала ядро под 24H2; MIT-проекты
используют ровно этот dual-path).

## 1. Как работает классика (Windows 10 → Windows 11 23H2)

1. `FindWindow("Progman", NULL)` — окно Program Manager.
2. `SendMessageTimeout(progman, 0x052C, 0xD, 0, SMTO_NORMAL, 1000, ...)` и
   повторно с `lParam = 1`. Недокументированное сообщение: Progman порождает
   WorkerW позади иконок (механизм анимации смены обоев). Идемпотентно — если
   WorkerW уже есть, сообщение игнорируется.
3. После этого среди top-level окон существуют **два** WorkerW:
   - WorkerW №1 — содержит `SHELLDLL_DefView` (сами иконки рабочего стола);
   - WorkerW №2 — пустой, лежит в Z-порядке **за** иконками. Это цель.
4. Поиск цели: `EnumWindows`; для каждого окна
   `FindWindowEx(hwnd, NULL, "SHELLDLL_DefView", NULL)`; когда DefView найден —
   цель = `FindWindowEx(NULL, hwnd_с_DefView, "WorkerW", NULL)` (следующий
   top-level WorkerW после него).
5. `SetParent(наше_окно, worker_w)` → наше окно рисуется позади иконок.

Нюанс старых сборок Win10: иногда `SHELLDLL_DefView` живёт прямо в Progman;
тогда часть приложений парентится напрямую к Progman — тоже работает.

## 2. Что сломалось в Windows 11 24H2 (build 26100) / 25H2 (26200)

Подтверждено несколькими независимыми источниками (Lively, GameMaker,
Wallpaper Engine, AutoHotkey-сообщество):

- **Иерархия изменилась**: после `0x052C` WorkerW создаётся как **child окна
  Progman** (за `SHELLDLL_DefView`, который также находится под Progman),
  а не top-level сиблингом. Классический поиск через `EnumWindows` находит ноль
  кандидатов.
- **Задержка создания**: сразу после логона WorkerW под Progman может ещё не
  существовать и появляется позже; `0x052C` форсирует создание, но нужен
  retry-цикл с небольшим таймаутом.
- **Пересоздание/уничтожение WorkerW**: смена системных обоев, перезапуск
  explorer.exe (и, по отчётам Lively, спонтанно) уничтожают WorkerW вместе с
  нашим окном-ребёнком → приложения без re-attach уходили в цикл перезапуска.
- Побочный эффект для фазы 1: на 24H2 у встроенного mpv-окна наблюдались
  артефакты «обои не на весь экран», лечится опцией mpv `border=no`
  (зафиксировать при интеграции libmpv).
- 25H2 (26200) делит servicing-ветку с 24H2 (одна платформа, один код шелла) —
  отдельных изменений механизма не обнаружено; ломаются только приложения без
  24H2-фолбэка.

Lively выпустила мажорную версию с «полностью переписанным ядром под 24H2» —
т.е. проблема решаема в рамках того же хака, отдельного API Microsoft не дала.

## 3. Выбранная стратегия (предложение)

Псевдокод поиска родителя (dual-path):

```text
find_wallpaper_parent():
    progman = FindWindow("Progman")
    SendMessageTimeout(progman, 0x052C, 0xD, 0)
    SendMessageTimeout(progman, 0x052C, 0xD, 1)

    retry до ~2 c (шаг 100 мс):                  # 24H2 создаёт WorkerW с задержкой
        # Путь A — 24H2+: WorkerW как child Progman
        w = FindWindowEx(progman, NULL, "WorkerW", NULL)
        if w: return w
        # Путь B — классика ≤23H2: сиблинг после окна с SHELLDLL_DefView
        w = EnumWindows: окно c SHELLDLL_DefView → FindWindowEx(NULL, оно, "WorkerW", NULL)
        if w: return w

    # Последний фолбэк (старые сборки Win10, где DefView прямо в Progman)
    return progman
```

Порядок A/B не критичен (на каждой сборке существует ровно один вариант);
проверяем оба на каждой итерации, без жёсткой привязки к номеру билда — меньше
шансов сломаться на будущих сборках.

Наше окно и позиционирование:

- Своё окно через crate `windows`: `WS_POPUP`, без рамки/заголовка,
  `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` (не активируется, нет в Alt-Tab).
- Процесс объявляет **Per-Monitor DPI v2**
  (`SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` первым делом в main) —
  все координаты в физических пикселях, без виртуализации.
- Клиентская область WorkerW покрывает весь виртуальный рабочий стол; после
  `SetParent` координаты монитора переводятся из экранных в клиентские
  WorkerW через `MapWindowPoints` (origin виртуального экрана может быть
  отрицательным — не считать руками, только MapWindowPoints).
- Мониторы: `EnumDisplayMonitors` + `GetMonitorInfo` (координаты, primary) +
  `GetDpiForMonitor` (масштаб).

Watchdog (обязателен из-за пересоздания WorkerW):

- Лёгкий таймер (~1 с): `IsWindow(parent)` и `IsWindow(progman)`; если родитель
  умер — повторить discovery и re-attach (пересоздать окно при необходимости).
  Это же покрывает перезапуск explorer.exe. CPU-цена — ноль (одно API-число в
  секунду), критерию «~0% CPU в покое» не противоречит.

Восстановление рабочего стола при выходе (Ctrl+C → graceful shutdown):

1. `DestroyWindow(наше_окно)`.
2. Перерисовка обоев: `SystemParametersInfo(SPI_SETDESKWALLPAPER, 0, NULL,
   SPIF_UPDATEINIFILE | SPIF_SENDCHANGE)` — переустанавливает текущие системные
   обои из реестра, убирает возможные артефакты на месте нашего окна.
3. Ctrl+C ловим через `SetConsoleCtrlHandler` (или crate `ctrlc`, MIT/Apache) и
   шлём команду завершения в цикл сообщений окна.

## 4. Известные проблемы (фиксируем заранее)

| Событие | Эффект | Митигация | Проверено |
|---|---|---|---|
| Смена системных обоев / персонализация | WorkerW может быть пересоздан, наше окно исчезает | watchdog + re-attach | Win10 22H2: окно выживает без re-attach (SPI_SETDESKWALLPAPER, 2026-07-10) |
| Перезапуск explorer.exe | Вся иерархия уничтожена | watchdog: ждать новый Progman, re-attach | не проверено (нужен ручной тест) |
| Логон/автостарт на 24H2 | WorkerW ещё не существует | retry-цикл в discovery | не проверено (нет 24H2-машины) |
| Смена разрешения / топологии мониторов | Старые координаты поверхности больше не совпадают с дисплеем | watchdog сопоставляет поверхность по имени устройства, меняет размер/позицию; скрывает отключённый монитор и возвращает при подключении | Win10 22H2: 1920x1080 → 1600x900 → 1920x1080, renderer выжил, размеры кадров совпали; hot-plug не проверен |
| F5 на рабочем столе | По отчётам — безвредно | тест в критериях приёмки | Win10 22H2: окно выживает (WM_COMMAND refresh, 2026-07-10) |
| Будущие сборки Windows | Иерархия может снова поменяться | dual-path без привязки к билду; абстракция WallpaperHost | — |

## 5. Лицензионная чистота

- Изучены **MIT/Apache** реализации: `meslzy/tauri-plugin-wallpaper` (MIT,
  dual-path discovery), `ohkashi/LiveWallpaper` (MIT), `yadokani389/bevy_live_wallpaper`
  (MIT/Apache). Идеи подтверждены, код не копировался — наша реализация пишется
  с нуля на crate `windows` (MIT/Apache-2.0).
- GPL-проекты (Lively): читались **только** issue-треки и release notes
  (симптомы 24H2, факт переписывания ядра). Исходный код не открывался.

## 6. Источники

- CodeProject «Draw Behind Desktop Icons in Windows 8+» — классика хака:
  <https://www.codeproject.com/Articles/856020/Draw-Behind-Desktop-Icons-in-Windows-plus>
- Описание техники (messages, поиск WorkerW, SetParent):
  <https://dynamicwallpaper.readthedocs.io/en/docs/dev/make-wallpaper.html>
- AutoHotkey: слом на 24H2, WorkerW под Progman, задержка создания:
  <https://www.autohotkey.com/boards/viewtopic.php?t=135199>
- MIT-реализация с 24H2-фолбэком: <https://github.com/meslzy/tauri-plugin-wallpaper>
- Lively: релиз с переписанным ядром под 24H2 и симптомы —
  <https://github.com/rocksdanister/lively/releases>,
  issues [#2374](https://github.com/rocksdanister/lively/issues/2374),
  [#2407](https://github.com/rocksdanister/lively/issues/2407) (WorkerW destroyed),
  [#2415](https://github.com/rocksdanister/lively/issues/2415) (border/не весь экран, mpv `border=no`),
  [#2422](https://github.com/rocksdanister/lively/issues/2422) (медленный старт).
- GameMaker: 24H2 ломает композицию wallpaper-окон:
  <https://github.com/YoYoGames/GameMaker-Bugs/issues/8710>
- 25H2: приложения без фолбэка ломаются (Wallpaper Alive, Steam-обсуждение):
  <https://steamcommunity.com/app/2003310/discussions/0/682986292644990155/>
