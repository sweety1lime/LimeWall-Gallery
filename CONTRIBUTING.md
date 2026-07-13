# Contributing to LimeWall

Thanks for helping with the beta! (Русские пометки — курсивом.)

## Reporting bugs — *баги*

Open an issue with the **Bug report / Баг** form. It asks for the **diagnostics
report** and your Windows version — that is what makes a bug fixable.

In the app: settings icon → **«Диагностика»** → **«Проверить»** → **«Скопировать
отчёт»**, then paste it into the form. *(Issues → «Баг / Bug report», приложи
отчёт диагностики и версию Windows — Win+R → `winver`.)*

## Ideas & questions — *идеи*

Use the **Feature request / Идея** form.

## Sharing wallpapers — *свои обои*

The catalog accepts **video and image packs only**, via Pull Request with
moderation — see [gallery/README.md](gallery/README.md). *(Только видео и
картинки, через PR.)*

## Building from source — *сборка*

See the [README](README.md) ([Русский](README.ru.md)): run the `scripts\fetch-*`
scripts once, then `scripts\build-portable.ps1`.

## Code conventions (for code PRs)

- English for code, identifiers, comments and commit messages.
- `cargo fmt` and `cargo clippy --workspace` clean; tests green.
- Keep platform-specific code inside `crates/platform`.
- Prefer permissively licensed dependencies (MIT / Apache-2.0 / BSD); do not copy
  in GPL code.
