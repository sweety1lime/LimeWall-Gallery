//! Media library: a data directory with imported media, jpg previews and a
//! JSON index. GIFs are never stored as-is — they are converted to mp4 on
//! import with the pinned LGPL ffmpeg (no libx264 there, so the encoder is
//! MediaFoundation H.264 with a VP9 software fallback).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const INDEX_FILE: &str = "index.json";
const PREVIEW_WIDTH: u32 = 320;

const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "mkv", "webm", "mov", "avi", "m4v"];
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "bmp", "webp"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Video,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: String,
    /// Display name taken from the original file stem.
    pub name: String,
    pub kind: MediaKind,
    /// Absolute path of the imported media inside the library.
    pub file: PathBuf,
    pub preview: Option<PathBuf>,
    /// Unix seconds.
    pub imported_at: u64,
}

pub struct Library {
    root: PathBuf,
}

impl Library {
    /// Library in the per-user data directory (%APPDATA%/LiveWall/library).
    pub fn default_location() -> Result<Self, String> {
        let data = dirs::data_dir().ok_or("no per-user data directory on this system")?;
        Ok(Self {
            root: data.join("LiveWall").join("library"),
        })
    }

    /// Library at an explicit root, for tests.
    #[cfg(test)]
    fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn list(&self) -> Result<Vec<LibraryItem>, String> {
        // Self-heal: hide entries whose media file disappeared.
        Ok(self
            .load_index()?
            .into_iter()
            .filter(|item| item.file.is_file())
            .collect())
    }

    pub fn import(&self, source: &Path) -> Result<LibraryItem, String> {
        let source = source
            .canonicalize()
            .map_err(|error| format!("file not found: {error}"))?;
        let extension = source
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .ok_or("file has no extension")?;
        let is_gif = extension == "gif";
        let kind = if is_gif || VIDEO_EXTENSIONS.contains(&extension.as_str()) {
            MediaKind::Video
        } else if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
            MediaKind::Image
        } else {
            return Err(format!("unsupported media type: .{extension}"));
        };

        let mut index = self.load_index()?;
        let id = content_id(&source)?;
        if let Some(existing) = index.iter().find(|item| item.id == id) {
            return Ok(existing.clone()); // already imported
        }

        fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        let target_extension = if is_gif { "mp4" } else { extension.as_str() };
        let target = self.root.join(format!("{id}.{target_extension}"));
        if is_gif {
            convert_gif_to_mp4(&source, &target)?;
        } else {
            fs::copy(&source, &target).map_err(|error| format!("copy failed: {error}"))?;
        }

        let preview = self.root.join(format!("{id}.jpg"));
        let preview = match generate_preview(&target, kind, &preview) {
            Ok(()) => Some(preview),
            Err(error) => {
                eprintln!("preview generation failed for {id}: {error}");
                None
            }
        };

        let item = LibraryItem {
            id,
            name: source
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unnamed".into()),
            kind,
            file: target,
            preview,
            imported_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        index.push(item.clone());
        self.save_index(&index)?;
        Ok(item)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut index = self.load_index()?;
        let Some(position) = index.iter().position(|item| item.id == id) else {
            return Err(format!("library item {id} not found"));
        };
        let item = index.remove(position);
        let _ = fs::remove_file(&item.file);
        if let Some(preview) = &item.preview {
            let _ = fs::remove_file(preview);
        }
        self.save_index(&index)
    }

    pub fn preview_jpeg(&self, id: &str) -> Result<Vec<u8>, String> {
        let index = self.load_index()?;
        let item = index
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| format!("library item {id} not found"))?;
        let preview = item.preview.as_ref().ok_or("item has no preview")?;
        fs::read(preview).map_err(|error| error.to_string())
    }

    fn index_path(&self) -> PathBuf {
        self.root.join(INDEX_FILE)
    }

    fn load_index(&self) -> Result<Vec<LibraryItem>, String> {
        match fs::read(self.index_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("library index is corrupted: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn save_index(&self, index: &[LibraryItem]) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        let json = serde_json::to_vec_pretty(index).map_err(|error| error.to_string())?;
        // Write-then-rename keeps the index readable if we crash mid-write.
        let temporary = self.index_path().with_extension("json.tmp");
        fs::write(&temporary, json).map_err(|error| error.to_string())?;
        fs::rename(&temporary, self.index_path()).map_err(|error| error.to_string())
    }
}

/// Stable item id: content hash, so re-importing the same file deduplicates.
fn content_id(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut id = String::with_capacity(16);
    for byte in &digest[..8] {
        id.push_str(&format!("{byte:02x}"));
    }
    Ok(id)
}

/// GIF -> mp4 per the project rules: yuv420p, even dimensions, +faststart.
/// The pinned LGPL ffmpeg has no libx264; MediaFoundation H.264 is the
/// primary encoder with software VP9 as fallback (both produce .mp4).
fn convert_gif_to_mp4(source: &Path, target: &Path) -> Result<(), String> {
    let encoders: [&[&str]; 2] = [
        &[
            "-c:v",
            "h264_mf",
            "-rate_control",
            "quality",
            "-quality",
            "70",
        ],
        &["-c:v", "libvpx-vp9", "-crf", "32", "-b:v", "0"],
    ];
    let mut last_error = String::new();
    for encoder in encoders {
        let mut command = ffmpeg_command()?;
        command
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(source)
            .args(["-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2"])
            .args(encoder)
            .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart", "-an"])
            .arg(target);
        match run_ffmpeg(command) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }
    Err(format!("gif conversion failed: {last_error}"))
}

/// One scaled jpg frame; for videos the frame is taken ~1s in when possible.
fn generate_preview(media: &Path, kind: MediaKind, target: &Path) -> Result<(), String> {
    let seeks: &[&[&str]] = match kind {
        MediaKind::Video => &[&["-ss", "1"], &[]], // short clips: retry at 0s
        MediaKind::Image => &[&[]],
    };
    let mut last_error = String::new();
    for seek in seeks {
        let mut command = ffmpeg_command()?;
        command.args(["-y", "-loglevel", "error"]);
        command.args(*seek);
        command
            .arg("-i")
            .arg(media)
            .args(["-frames:v", "1"])
            .args(["-vf", &format!("scale={PREVIEW_WIDTH}:-2")])
            .args(["-q:v", "4"])
            .arg(target);
        match run_ffmpeg(command) {
            Ok(()) if target.is_file() => return Ok(()),
            Ok(()) => last_error = "ffmpeg produced no output".into(),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

fn ffmpeg_command() -> Result<Command, String> {
    let ffmpeg = ffmpeg_path().ok_or_else(|| {
        "ffmpeg not found: run scripts/fetch-ffmpeg.ps1 or set LIVEWALL_FFMPEG".to_owned()
    })?;
    let mut command = Command::new(ffmpeg);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(command)
}

fn run_ffmpeg(mut command: Command) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|error| format!("failed to run ffmpeg: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(stderr.lines().last().unwrap_or("ffmpeg failed").to_owned())
    }
}

/// ffmpeg lookup: explicit override, next to the UI executable (bundled
/// install), then the development checkout download location.
fn ffmpeg_path() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let mut candidates = Vec::new();
    if let Ok(explicit) = std::env::var("LIVEWALL_FFMPEG") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Ok(ui_exe) = std::env::current_exe()
        && let Some(dir) = ui_exe.parent()
    {
        candidates.push(dir.join(exe_name));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../third_party/ffmpeg/unpacked")
            .join(exe_name),
    );
    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generates disposable inputs with ffmpeg itself (lavfi test sources).
    fn generate(args: &[&str], target: &Path) {
        let mut command = ffmpeg_command().expect("ffmpeg must be fetched for tests");
        command.args(["-y", "-loglevel", "error"]);
        command.args(args);
        command.arg(target);
        run_ffmpeg(command).expect("test input generation failed");
    }

    #[test]
    fn imports_gif_as_mp4_with_preview_and_dedup() {
        if ffmpeg_path().is_none() {
            eprintln!("skipped: ffmpeg not fetched");
            return;
        }
        let temp = tempfile::tempdir().expect("temp dir");
        let library = Library::at(temp.path().join("library"));

        let gif = temp.path().join("bounce.gif");
        generate(
            &["-f", "lavfi", "-i", "testsrc=duration=1:size=64x48:rate=10"],
            &gif,
        );

        let item = library.import(&gif).expect("gif import");
        assert_eq!(item.kind, MediaKind::Video);
        assert_eq!(item.file.extension().unwrap(), "mp4");
        assert!(item.file.is_file(), "converted mp4 must exist");
        assert!(item.preview.as_ref().is_some_and(|p| p.is_file()));
        assert_eq!(item.name, "bounce");

        // Re-import of identical content must return the same item.
        let again = library.import(&gif).expect("second import");
        assert_eq!(again.id, item.id);
        assert_eq!(library.list().expect("list").len(), 1);

        let preview = library.preview_jpeg(&item.id).expect("preview bytes");
        assert!(!preview.is_empty());

        library.remove(&item.id).expect("remove");
        assert!(library.list().expect("list").is_empty());
        assert!(!item.file.exists(), "media file must be deleted");
    }

    #[test]
    fn imports_image_by_copy() {
        if ffmpeg_path().is_none() {
            eprintln!("skipped: ffmpeg not fetched");
            return;
        }
        let temp = tempfile::tempdir().expect("temp dir");
        let library = Library::at(temp.path().join("library"));

        let png = temp.path().join("wall.png");
        generate(
            &[
                "-f",
                "lavfi",
                "-i",
                "color=red:size=64x48",
                "-frames:v",
                "1",
            ],
            &png,
        );

        let item = library.import(&png).expect("png import");
        assert_eq!(item.kind, MediaKind::Image);
        assert_eq!(item.file.extension().unwrap(), "png");
        assert!(item.preview.as_ref().is_some_and(|p| p.is_file()));
    }

    #[test]
    fn rejects_unknown_extensions() {
        let temp = tempfile::tempdir().expect("temp dir");
        let library = Library::at(temp.path().join("library"));
        let odd = temp.path().join("notes.txt");
        fs::write(&odd, "hello").expect("write");
        let error = library.import(&odd).expect_err("txt must be rejected");
        assert!(error.contains("unsupported media type"));
    }
}
