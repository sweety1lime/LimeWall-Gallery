//! Self-diagnostics: read-only checks the panel runs to explain "why isn't my
//! wallpaper showing". Daemon facts come from existing read-only IPC (Ping /
//! ListMonitors / Status / GetAutostart); file and shell checks run here. It
//! deliberately never spawns the daemon — a down daemon must stay diagnosable.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::daemon_client;
use crate::library;

#[derive(serde::Serialize)]
pub struct DiagCheck {
    /// Stable English slug; the frontend maps it to a Russian label.
    pub id: String,
    /// "pass" | "fail" | "warn" | "info".
    pub status: String,
    pub detail: String,
}

#[derive(serde::Serialize)]
pub struct DiagReport {
    pub checks: Vec<DiagCheck>,
    pub log_tail: String,
    pub ui_version: String,
}

fn check(id: &str, status: &str, detail: impl Into<String>) -> DiagCheck {
    DiagCheck {
        id: id.into(),
        status: status.into(),
        detail: detail.into(),
    }
}

#[tauri::command]
pub async fn run_diagnostics() -> Result<DiagReport, String> {
    crate::blocking(|| Ok(collect())).await
}

fn collect() -> DiagReport {
    let mut checks = Vec::new();
    let endpoint = daemon_client::endpoint();

    // Daemon: raw ping, never `ensure_daemon` — diagnostics observe, not spawn.
    match daemon_client::request(&endpoint, ipc::Command::Ping) {
        Ok(ipc::ResponseData::Pong { daemon_version }) => checks.push(check(
            "daemon",
            "pass",
            format!("работает, версия {daemon_version}"),
        )),
        Ok(other) => checks.push(check(
            "daemon",
            "warn",
            format!("неожиданный ответ: {other:?}"),
        )),
        Err(error) => checks.push(check("daemon", "fail", format!("нет ответа: {error}"))),
    }

    // Renderer executable.
    match daemon_client::renderer_path() {
        Some(path) => checks.push(check("renderer_exe", "pass", path.display().to_string())),
        None => checks.push(check(
            "renderer_exe",
            "fail",
            "renderer.exe не найден рядом с приложением",
        )),
    }

    // libmpv-2.dll (needed to play video).
    match libmpv_path() {
        Some(path) => checks.push(check(
            "libmpv",
            "pass",
            format!("{} (загрузку проверяет плеер при старте)", path.display()),
        )),
        None => checks.push(check(
            "libmpv",
            "warn",
            "libmpv-2.dll не найдена — запустите scripts/fetch-libmpv.ps1 или положите рядом с renderer.exe",
        )),
    }

    // ffmpeg (needed to import video / convert GIF).
    match library::ffmpeg_path() {
        Some(path) => checks.push(check("ffmpeg", "pass", path.display().to_string())),
        None => checks.push(check(
            "ffmpeg",
            "warn",
            "ffmpeg не найден — импорт видео и GIF будет недоступен",
        )),
    }

    // Monitors (via the daemon).
    match daemon_client::request(&endpoint, ipc::Command::ListMonitors) {
        Ok(ipc::ResponseData::Monitors { monitors }) if !monitors.is_empty() => {
            let names: Vec<String> = monitors
                .iter()
                .map(|m| format!("{}×{}", m.bounds.width, m.bounds.height))
                .collect();
            checks.push(check(
                "monitors",
                "pass",
                format!("{} шт.: {}", monitors.len(), names.join(", ")),
            ));
        }
        Ok(_) => checks.push(check("monitors", "fail", "мониторы не обнаружены")),
        Err(error) => checks.push(check("monitors", "fail", format!("нет ответа: {error}"))),
    }

    // Desktop icons visible — on Windows 11 24H2 hidden icons make the
    // wallpaper layer invisible.
    match platform::desktop_icons_visible() {
        Some(true) => checks.push(check("desktop_icons", "pass", "видимы")),
        Some(false) => checks.push(check(
            "desktop_icons",
            "fail",
            "скрыты — на Windows 11 24H2 обои не показываются, пока иконки скрыты (ПКМ по столу → Вид → Отображать значки)",
        )),
        None => checks.push(check(
            "desktop_icons",
            "warn",
            "не удалось определить (возможно, перезапускается проводник)",
        )),
    }

    // Active wallpapers.
    match daemon_client::request(&endpoint, ipc::Command::Status) {
        Ok(ipc::ResponseData::Status {
            sessions,
            stack_cpu_percent,
            ..
        }) => {
            let detail = if sessions.is_empty() {
                "нет активных обоев".to_string()
            } else {
                let parts: Vec<String> = sessions
                    .iter()
                    .map(|s| format!("монитор {}: {}", s.monitor, state_word(s.state)))
                    .collect();
                let cpu = stack_cpu_percent
                    .map(|p| format!(" · {p:.0}% CPU"))
                    .unwrap_or_default();
                format!("{}{cpu}", parts.join("; "))
            };
            checks.push(check("sessions", "info", detail));
        }
        Ok(_) | Err(_) => checks.push(check("sessions", "info", "недоступно (плеер не отвечает)")),
    }

    // Autostart.
    match daemon_client::request(&endpoint, ipc::Command::GetAutostart) {
        Ok(ipc::ResponseData::Autostart { enabled }) => checks.push(check(
            "autostart",
            "info",
            if enabled {
                "включён"
            } else {
                "выключен"
            },
        )),
        Ok(_) | Err(_) => checks.push(check("autostart", "warn", "не удалось прочитать")),
    }

    // Daemon log presence.
    let log_path = daemon_log_path();
    let log_exists = log_path.as_ref().is_some_and(|p| p.is_file());
    checks.push(check(
        "daemon_log",
        if log_exists { "info" } else { "warn" },
        match &log_path {
            Some(p) if log_exists => p.display().to_string(),
            _ => "журнал ещё не создан".to_string(),
        },
    ));

    let log_tail = log_path
        .as_deref()
        .map(|p| read_log_tail(p, 64 * 1024, 60))
        .unwrap_or_default();

    DiagReport {
        checks,
        log_tail,
        ui_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn state_word(state: ipc::PlaybackState) -> &'static str {
    match state {
        ipc::PlaybackState::Playing => "играет",
        ipc::PlaybackState::Paused => "на паузе",
        ipc::PlaybackState::Stopped => "остановлено",
    }
}

/// libmpv-2.dll next to the renderer exe (bundled) or in the dev checkout.
fn libmpv_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) =
        daemon_client::renderer_path().and_then(|p| p.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join("libmpv-2.dll"));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../third_party/mpv/unpacked/libmpv-2.dll"),
    );
    candidates.into_iter().find(|p| p.is_file())
}

fn daemon_log_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("LimeWall").join("daemon.log"))
}

/// Last `max_lines` lines of the file, reading at most `max_bytes` from the end
/// so a huge log never loads whole.
fn read_log_tail(path: &Path, max_bytes: u64, max_lines: usize) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max_bytes {
        let _ = file.seek(SeekFrom::Start(len - max_bytes));
    }
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    tail_lines(&String::from_utf8_lossy(&bytes), max_lines)
}

/// Last `max_lines` lines of `text`.
fn tail_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_lines_keeps_only_the_last_lines() {
        assert_eq!(tail_lines("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(tail_lines("only", 5), "only");
        assert_eq!(tail_lines("", 5), "");
    }

    #[test]
    fn read_log_tail_handles_missing_file_and_bounds_bytes() {
        let dir = std::env::temp_dir().join(format!("limewall-diag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("nope.log");
        assert_eq!(read_log_tail(&missing, 1024, 10), "");

        let log = dir.join("d.log");
        std::fs::write(&log, "l1\nl2\nl3\nl4\nl5").unwrap();
        assert_eq!(read_log_tail(&log, 1024, 2), "l4\nl5");
        std::fs::remove_dir_all(&dir).ok();
    }
}
