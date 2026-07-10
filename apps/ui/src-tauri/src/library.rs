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
    /// Display name: the original file stem, or the package name.
    pub name: String,
    pub kind: MediaKind,
    /// Absolute path of the imported media inside the library.
    pub file: PathBuf,
    pub preview: Option<PathBuf>,
    /// Unix seconds.
    pub imported_at: u64,
    /// Metadata carried by .wpk packages; None for plain file imports.
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
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
        if source
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("wpk"))
        {
            return self.import_wpk(&source);
        }
        self.import_media(&source, None, None, None, None)
    }

    /// Unpacks a `.wpk` package and imports its entry, keeping the manifest
    /// name, author and license.
    fn import_wpk(&self, package: &Path) -> Result<LibraryItem, String> {
        let manifest = wpk::read_manifest(package).map_err(|error| error.to_string())?;
        match manifest.media_type {
            wpk::MediaType::Video | wpk::MediaType::Image => {}
            wpk::MediaType::Web | wpk::MediaType::Model3d => {
                return Err("web and 3D packages are not supported yet (phase 6)".into());
            }
        }
        // The package id is the identity across libraries — but it becomes a
        // file name here, so anything unusual falls back to a content hash.
        let package_id = Some(manifest.id.clone()).filter(|id| is_safe_id(id));
        // Unpack next to the library so the rename-free import can read it.
        let staging = self.root.join(".staging");
        let staged_entry = staging.join(&manifest.entry);
        wpk::extract_file(package, &manifest.entry, &staged_entry)
            .map_err(|error| error.to_string())?;
        let result = self.import_media(
            &staged_entry,
            Some(manifest.name.clone()),
            Some(manifest.author.clone()),
            Some(manifest.license.clone()),
            package_id,
        );
        let _ = fs::remove_dir_all(&staging);
        result
    }

    /// Exports a library item as a `.wpk` package.
    pub fn export(&self, id: &str, target: &Path) -> Result<(), String> {
        let index = self.load_index()?;
        let item = index
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| format!("library item {id} not found"))?;
        let entry = item
            .file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("library file has no usable name")?
            .to_owned();
        let preview_name = item
            .preview
            .as_ref()
            .filter(|path| path.is_file())
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        let manifest = wpk::Manifest {
            id: item.id.clone(),
            media_type: match item.kind {
                MediaKind::Video => wpk::MediaType::Video,
                MediaKind::Image => wpk::MediaType::Image,
            },
            entry: entry.clone(),
            name: item.name.clone(),
            author: item.author.clone().unwrap_or_else(|| "unknown".into()),
            license: item.license.clone().unwrap_or_else(|| "unknown".into()),
            version: "1.0".into(),
            preview: preview_name.clone(),
            options: serde_json::Map::new(),
        };
        let mut files: Vec<(&str, &Path)> = vec![(entry.as_str(), item.file.as_path())];
        if let (Some(name), Some(path)) = (&preview_name, &item.preview) {
            files.push((name.as_str(), path.as_path()));
        }
        wpk::write_package(target, &manifest, &files).map_err(|error| error.to_string())
    }

    fn import_media(
        &self,
        source: &Path,
        name: Option<String>,
        author: Option<String>,
        license: Option<String>,
        forced_id: Option<String>,
    ) -> Result<LibraryItem, String> {
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
        let id = match forced_id {
            Some(id) => id,
            None => content_id(source)?,
        };
        if let Some(existing) = index.iter().find(|item| item.id == id) {
            return Ok(existing.clone()); // already imported
        }

        fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        let target_extension = if is_gif { "mp4" } else { extension.as_str() };
        let target = self.root.join(format!("{id}.{target_extension}"));
        if is_gif {
            convert_gif_to_mp4(source, &target)?;
        } else {
            fs::copy(source, &target).map_err(|error| format!("copy failed: {error}"))?;
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
            name: name.unwrap_or_else(|| {
                source
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "unnamed".into())
            }),
            kind,
            file: target,
            preview,
            imported_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            author,
            license,
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

/// Package ids double as file names; anything beyond this charset is unsafe.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
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
    fn wpk_export_import_round_trips() {
        if ffmpeg_path().is_none() {
            eprintln!("skipped: ffmpeg not fetched");
            return;
        }
        let temp = tempfile::tempdir().expect("temp dir");
        let library = Library::at(temp.path().join("library"));

        let gif = temp.path().join("loop.gif");
        generate(
            &["-f", "lavfi", "-i", "testsrc=duration=1:size=64x48:rate=10"],
            &gif,
        );
        let item = library.import(&gif).expect("gif import");

        let package = temp.path().join("loop.wpk");
        library.export(&item.id, &package).expect("export");
        let manifest = wpk::read_manifest(&package).expect("exported package is valid");
        assert_eq!(manifest.id, item.id);
        assert_eq!(manifest.name, "loop");
        assert_eq!(manifest.author, "unknown");

        // Import into a fresh library keeps identity and metadata.
        let second = Library::at(temp.path().join("library2"));
        let restored = second.import(&package).expect("wpk import");
        assert_eq!(restored.id, item.id);
        assert_eq!(restored.name, "loop");
        assert_eq!(restored.author.as_deref(), Some("unknown"));
        assert!(restored.file.is_file());
        assert_eq!(
            std::fs::read(&restored.file).expect("restored media"),
            std::fs::read(&item.file).expect("original media"),
            "package must carry the converted mp4 byte for byte"
        );
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
