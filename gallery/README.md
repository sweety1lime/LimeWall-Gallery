# Каталог обоев LimeWall

Это встроенная галерея: приложение читает `catalog.json` отсюда и предлагает паки
к скачиванию. Публикация — через Pull Request с ревью (модерация).

## Что принимается (v1)

- **Только видео и картинки** (`type: video | image`). Интерактивные web/3D-обои
  пока **не принимаются** — их нельзя безопасно проверить автоматически.
- Пак в формате `.wpk` (см. `crates/wpk`), с корректным `manifest.json`
  (`author`, `license`, `version` обязательны).
- **Права на контент.** Публикуйте только своё или то, что разрешает лицензия.
  Укажите её честно в `license`. Нарушения удаляются (revocation + DMCA GitHub).
- Без NSFW, рекламы, вредоносного и вводящего в заблуждение содержимого.

## Как добавить свой пак

1. **Соберите `.wpk`** из видео/картинки одной командой — она сразу печатает
   SHA-256, размер и готовую запись каталога:

   ```
   cargo run -p renderer -- pack wall.mp4 --name "Aurora Drift" --author "2fame" --license "CC-BY-4.0" --preview preview.jpg
   ```

   (Или экспортом из приложения — кнопка «Поделиться .wpk» на карточке.)
2. **Захостьте `.wpk`**: приложите его к **GitHub Release** этого репозитория
   (или своего форка) — тогда `download_url` будет вида
   `https://github.com/.../releases/download/<tag>/<file>.wpk`.
3. **Добавьте запись** из шага 1 в `gallery/catalog.json` (подставив реальный
   `download_url`) и положите превью в `gallery/packs/<id>/preview.jpg`.
4. **Откройте Pull Request.** Пройдут автопроверки (`validate.mjs` — структура,
   `verify-downloads.mjs` — совпадение SHA-256 с файлом), затем ревью — и после
   мержа пак появится в каталоге у всех.

## Формат записи каталога

```json
{
  "id": "aurora-drift",
  "name": "Aurora Drift",
  "author": "2fame",
  "type": "video",
  "license": "CC-BY-4.0",
  "sha256": "<64 hex>",
  "size": 12345678,
  "preview": "https://raw.githubusercontent.com/sweety1lime/LimeWall-Gallery/master/gallery/packs/aurora-drift/preview.jpg",
  "download_url": "https://github.com/sweety1lime/LimeWall-Gallery/releases/download/aurora-drift/aurora-drift.wpk",
  "tags": ["abstract", "green"]
}
```

Проверить локально перед PR: `node gallery/validate.mjs`.

## Безопасность

Скачанный пак приложение проверяет по **SHA-256** из каталога перед установкой, и
качает только с `github.com` / `raw.githubusercontent.com`. Отзыв пака —
удаление Release + запись в revocation-list. Подробнее: `docs/research/workshop.md`.
