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
    /// HTML / three.js wallpaper: a folder under the library, `file` is the
    /// entry page.
    Web,
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
    /// Library in the per-user data directory (%APPDATA%/LimeWall/library).
    pub fn default_location() -> Result<Self, String> {
        let data = dirs::data_dir().ok_or("no per-user data directory on this system")?;
        Ok(Self {
            root: data.join("LimeWall").join("library"),
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
            .unwrap_or_default();
        if extension == "wpk" {
            return self.import_wpk(&source);
        }
        // A bare HTML entry: import its whole folder as a web wallpaper.
        if extension == "html" || extension == "htm" {
            return self.import_web_folder(&source);
        }
        self.import_media(&source, None, None, None, None)
    }

    /// Unpacks a `.wpk` package and imports it, keeping the manifest name,
    /// author and license.
    fn import_wpk(&self, package: &Path) -> Result<LibraryItem, String> {
        let manifest = wpk::read_manifest(package).map_err(|error| error.to_string())?;
        let package_id = Some(manifest.id.clone()).filter(|id| is_safe_id(id));
        match manifest.media_type {
            wpk::MediaType::Video | wpk::MediaType::Image => {
                // Unpack just the entry next to the library for the rename-free
                // single-file import.
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
            wpk::MediaType::Web | wpk::MediaType::Model3d => {
                let id = package_id.unwrap_or_else(|| content_id(package).unwrap_or_default());
                let dir = self.web_item_dir(&id);
                let mut index = self.load_index()?;
                if let Some(existing) = index.iter().find(|item| item.id == id) {
                    return Ok(existing.clone());
                }
                let _ = fs::remove_dir_all(&dir);
                wpk::extract_all(package, &dir).map_err(|error| error.to_string())?;
                let entry = dir.join(&manifest.entry);
                if !entry.is_file() {
                    let _ = fs::remove_dir_all(&dir);
                    return Err(format!("package entry {} is missing", manifest.entry));
                }
                let preview = manifest
                    .preview
                    .as_ref()
                    .map(|p| dir.join(p))
                    .filter(|p| p.is_file());
                let item = self.web_item(
                    id,
                    manifest.name.clone(),
                    entry,
                    preview,
                    Some(manifest.author.clone()),
                    Some(manifest.license.clone()),
                );
                index.push(item.clone());
                self.save_index(&index)?;
                Ok(item)
            }
        }
    }

    /// Imports a bare HTML file by copying its containing folder into the
    /// library as a web wallpaper. Guards against packaging a huge folder.
    fn import_web_folder(&self, entry: &Path) -> Result<LibraryItem, String> {
        let folder = entry.parent().ok_or("HTML file has no parent folder")?;
        let entry_name = entry
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("bad entry file name")?
            .to_owned();
        let id = content_id(entry)?;
        let dir = self.web_item_dir(&id);
        let mut index = self.load_index()?;
        if let Some(existing) = index.iter().find(|item| item.id == id) {
            return Ok(existing.clone());
        }
        let _ = fs::remove_dir_all(&dir);
        copy_folder_guarded(folder, &dir)?;
        let name = entry
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("web wallpaper")
            .to_owned();
        let item = self.web_item(id, name, dir.join(&entry_name), None, None, None);
        index.push(item.clone());
        self.save_index(&index)?;
        Ok(item)
    }

    fn web_item_dir(&self, id: &str) -> PathBuf {
        self.root.join(format!("web-{id}"))
    }

    #[allow(clippy::too_many_arguments)]
    fn web_item(
        &self,
        id: String,
        name: String,
        entry: PathBuf,
        preview: Option<PathBuf>,
        author: Option<String>,
        license: Option<String>,
    ) -> LibraryItem {
        LibraryItem {
            id,
            name,
            kind: MediaKind::Web,
            file: entry,
            preview,
            imported_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            author,
            license,
        }
    }

    /// Exports a library item as a `.wpk` package.
    pub fn export(&self, id: &str, target: &Path) -> Result<(), String> {
        let index = self.load_index()?;
        let item = index
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| format!("library item {id} not found"))?
            .clone();
        if item.kind == MediaKind::Web {
            return self.export_web(&item, target);
        }
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
                MediaKind::Web => unreachable!("web handled above"),
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

    /// Packs a web item's whole folder back into a `.wpk`.
    fn export_web(&self, item: &LibraryItem, target: &Path) -> Result<(), String> {
        let dir = item
            .file
            .parent()
            .ok_or("web item has no folder")?
            .to_path_buf();
        let entry = item
            .file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("bad entry name")?
            .to_owned();
        // Collect every file under the folder as forward-slash archive names.
        let mut collected: Vec<(String, PathBuf)> = Vec::new();
        collect_files(&dir, &dir, &mut collected)?;
        let manifest = wpk::Manifest {
            id: item.id.clone(),
            media_type: wpk::MediaType::Web,
            entry: entry.clone(),
            name: item.name.clone(),
            author: item.author.clone().unwrap_or_else(|| "unknown".into()),
            license: item.license.clone().unwrap_or_else(|| "unknown".into()),
            version: "1.0".into(),
            preview: item
                .preview
                .as_ref()
                .and_then(|p| p.strip_prefix(&dir).ok())
                .map(|p| p.to_string_lossy().replace('\\', "/")),
            options: serde_json::Map::new(),
        };
        let files: Vec<(&str, &Path)> = collected
            .iter()
            .map(|(name, path)| (name.as_str(), path.as_path()))
            .collect();
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
        if item.kind == MediaKind::Web {
            // Web items live in their own folder under the library root.
            if let Some(dir) = item.file.parent()
                && dir != self.root
                && dir.starts_with(&self.root)
            {
                let _ = fs::remove_dir_all(dir);
            }
        } else {
            let _ = fs::remove_file(&item.file);
            if let Some(preview) = &item.preview {
                let _ = fs::remove_file(preview);
            }
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

/// Caps for packaging or copying a web wallpaper folder.
const MAX_WEB_FILES: usize = 4096;
const MAX_WEB_BYTES: u64 = 512 * 1024 * 1024;

/// Recursively copies `from` into `to`, refusing an unexpectedly large folder.
fn copy_folder_guarded(from: &Path, to: &Path) -> Result<(), String> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(from, from, &mut files)?;
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    let mut total = 0u64;
    for (relative, source) in files {
        let dest = to.join(&relative);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = fs::copy(&source, &dest).map_err(|e| e.to_string())?;
        total += bytes;
        if total > MAX_WEB_BYTES {
            let _ = fs::remove_dir_all(to);
            return Err("web folder is too large (over 512 MB)".into());
        }
    }
    Ok(())
}

/// Collects every file under `root` as `(forward/slash/relative, absolute)`.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else if path.is_file() {
            if out.len() >= MAX_WEB_FILES {
                return Err("web folder has too many files".into());
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            out.push((relative, path));
        }
    }
    Ok(())
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
            "80",
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
        // Web previews come from the package manifest, not ffmpeg.
        MediaKind::Web => return Err("no ffmpeg preview for web".into()),
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
        "ffmpeg not found: run scripts/fetch-ffmpeg.ps1 or set LIMEWALL_FFMPEG".to_owned()
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
    if let Ok(explicit) = std::env::var("LIMEWALL_FFMPEG") {
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

    #[test]
    fn web_folder_imports_exports_and_removes() {
        // No ffmpeg needed for web wallpapers.
        let temp = tempfile::tempdir().expect("temp dir");
        let library = Library::at(temp.path().join("library"));

        // A tiny web wallpaper: entry plus a subfolder asset.
        let site = temp.path().join("aurora");
        fs::create_dir_all(site.join("assets")).expect("mkdir");
        fs::write(site.join("index.html"), b"<html><body>hi</body></html>").expect("entry");
        fs::write(site.join("assets/app.js"), b"console.log(1)").expect("asset");

        let item = library.import(&site.join("index.html")).expect("web import");
        assert_eq!(item.kind, MediaKind::Web);
        assert!(item.file.ends_with("index.html"));
        assert!(item.file.is_file(), "entry copied into the library");
        // The whole folder came along.
        assert!(item.file.parent().unwrap().join("assets/app.js").is_file());

        // Export to a package and re-import into a fresh library.
        let package = temp.path().join("aurora.wpk");
        library.export(&item.id, &package).expect("export web");
        let manifest = wpk::read_manifest(&package).expect("valid package");
        assert_eq!(manifest.media_type, wpk::MediaType::Web);
        assert_eq!(manifest.entry, "index.html");

        let second = Library::at(temp.path().join("library2"));
        let restored = second.import(&package).expect("web wpk import");
        assert_eq!(restored.kind, MediaKind::Web);
        assert!(restored.file.parent().unwrap().join("assets/app.js").is_file());

        // Removal deletes the whole web folder.
        let dir = item.file.parent().unwrap().to_path_buf();
        library.remove(&item.id).expect("remove");
        assert!(!dir.exists(), "web folder deleted");
        assert!(library.list().expect("list").is_empty());
    }
}
