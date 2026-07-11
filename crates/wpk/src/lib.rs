//! `.wpk` wallpaper package: a zip container holding `manifest.json` plus the
//! content files. The manifest schema is fixed from format v1 on — author,
//! license and version are mandatory because the future gallery and paid
//! packs depend on them.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

pub const MANIFEST_NAME: &str = "manifest.json";
/// Manifests larger than this are rejected as hostile.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// Zip-bomb guard for single extracted files.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Video,
    Image,
    Web,
    Model3d,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    #[serde(rename = "type")]
    pub media_type: MediaType,
    /// Archive name of the main content file.
    pub entry: String,
    pub name: String,
    pub author: String,
    pub license: String,
    pub version: String,
    /// Archive name of an optional preview image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Free-form per-type options; empty for plain media.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub options: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum WpkError {
    #[error("failed to access the package: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a valid zip archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("manifest.json is missing from the package")]
    MissingManifest,
    #[error("manifest.json is not valid JSON: {0}")]
    ManifestJson(#[from] serde_json::Error),
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    #[error("package file {0:?} is missing from the archive")]
    MissingFile(String),
    #[error("unsafe archive member name: {0:?}")]
    UnsafeName(String),
    #[error("archive member {name:?} exceeds {limit} bytes")]
    TooLarge { name: String, limit: u64 },
}

pub type Result<T> = std::result::Result<T, WpkError>;

/// Reads and fully validates the manifest of a package: schema fields,
/// safe member names, and presence of the referenced files.
pub fn read_manifest(package: &Path) -> Result<Manifest> {
    let mut archive = ZipArchive::new(File::open(package)?)?;
    let manifest: Manifest = {
        let mut file = match archive.by_name(MANIFEST_NAME) {
            Ok(file) => file,
            Err(zip::result::ZipError::FileNotFound) => return Err(WpkError::MissingManifest),
            Err(error) => return Err(error.into()),
        };
        // Cap the actual decompressed bytes, not the header-declared size:
        // a hostile archive can understate its size to slip past a check.
        let bytes = read_capped(&mut file, MAX_MANIFEST_BYTES, MANIFEST_NAME)?;
        serde_json::from_slice(&bytes)?
    };
    validate_manifest(&manifest)?;
    for name in referenced_files(&manifest) {
        if archive.by_name(&name).is_err() {
            return Err(WpkError::MissingFile(name));
        }
    }
    Ok(manifest)
}

/// Extracts one archive member to `target` (used for the manifest entry and
/// preview). Member names are validated against path traversal.
pub fn extract_file(package: &Path, member: &str, target: &Path) -> Result<()> {
    ensure_safe_name(member)?;
    let mut archive = ZipArchive::new(File::open(package)?)?;
    let mut file = archive
        .by_name(member)
        .map_err(|_| WpkError::MissingFile(member.to_owned()))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut output = File::create(target)?;
    // Cap by the bytes actually written, so a zip bomb cannot fill the disk
    // by lying about its uncompressed size in the header.
    if let Err(error) = copy_capped(&mut file, &mut output, MAX_FILE_BYTES, member) {
        drop(output);
        let _ = std::fs::remove_file(target);
        return Err(error);
    }
    output.flush()?;
    Ok(())
}

/// Writes a package: the manifest plus `(archive_name, source_path)` files.
pub fn write_package(target: &Path, manifest: &Manifest, files: &[(&str, &Path)]) -> Result<()> {
    validate_manifest(manifest)?;
    for name in referenced_files(manifest) {
        if !files.iter().any(|(file_name, _)| *file_name == name) {
            return Err(WpkError::MissingFile(name));
        }
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut writer = ZipWriter::new(File::create(target)?);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer.start_file(MANIFEST_NAME, options)?;
    writer.write_all(&serde_json::to_vec_pretty(manifest)?)?;
    for (name, source) in files {
        ensure_safe_name(name)?;
        // Media is already compressed; deflate would only burn CPU.
        let stored = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        writer.start_file(*name, stored)?;
        let mut input = File::open(source)?;
        std::io::copy(&mut input, &mut writer)?;
    }
    writer.finish()?.flush()?;
    Ok(())
}

fn referenced_files(manifest: &Manifest) -> Vec<String> {
    let mut names = vec![manifest.entry.clone()];
    if let Some(preview) = &manifest.preview {
        names.push(preview.clone());
    }
    names
}

fn validate_manifest(manifest: &Manifest) -> Result<()> {
    let required = [
        ("id", &manifest.id),
        ("entry", &manifest.entry),
        ("name", &manifest.name),
        ("author", &manifest.author),
        ("license", &manifest.license),
        ("version", &manifest.version),
    ];
    for (field, value) in required {
        if value.trim().is_empty() {
            return Err(WpkError::InvalidManifest(format!(
                "field {field:?} must not be empty"
            )));
        }
    }
    ensure_safe_name(&manifest.entry)?;
    if let Some(preview) = &manifest.preview {
        ensure_safe_name(preview)?;
    }
    Ok(())
}

/// Reads at most `limit` bytes; errors if the stream has more. Guards against
/// archives whose header understates the real decompressed size.
fn read_capped(reader: &mut impl Read, limit: u64, name: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let read = reader.take(limit + 1).read_to_end(&mut bytes)?;
    if read as u64 > limit {
        return Err(WpkError::TooLarge {
            name: name.to_owned(),
            limit,
        });
    }
    Ok(bytes)
}

/// Copies at most `limit` bytes from `reader` to `writer`; errors if the
/// stream has more (zip-bomb guard by actual output, not the header value).
fn copy_capped(
    reader: &mut impl Read,
    writer: &mut impl Write,
    limit: u64,
    name: &str,
) -> Result<()> {
    let copied = std::io::copy(&mut reader.take(limit + 1), writer)?;
    if copied > limit {
        return Err(WpkError::TooLarge {
            name: name.to_owned(),
            limit,
        });
    }
    Ok(())
}

/// Flat, relative, forward-slash names only — no traversal, no drives.
fn ensure_safe_name(name: &str) -> Result<()> {
    let unsafe_name = || WpkError::UnsafeName(name.to_owned());
    if name.is_empty() || name.contains('\\') || name.contains(':') {
        return Err(unsafe_name());
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(unsafe_name());
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(unsafe_name());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> Manifest {
        Manifest {
            id: "0011223344556677".into(),
            media_type: MediaType::Video,
            entry: "wall.mp4".into(),
            name: "Sample wall".into(),
            author: "unknown".into(),
            license: "unknown".into(),
            version: "1.0".into(),
            preview: Some("preview.jpg".into()),
            options: serde_json::Map::new(),
        }
    }

    fn write_sample(dir: &Path) -> std::path::PathBuf {
        let media = dir.join("wall.mp4");
        std::fs::write(&media, b"not really a video").expect("write media");
        let preview = dir.join("preview.jpg");
        std::fs::write(&preview, b"not really a jpeg").expect("write preview");
        let package = dir.join("sample.wpk");
        write_package(
            &package,
            &sample_manifest(),
            &[
                ("wall.mp4", media.as_path()),
                ("preview.jpg", preview.as_path()),
            ],
        )
        .expect("write package");
        package
    }

    #[test]
    fn package_round_trips() {
        let temp = tempfile::tempdir().expect("temp dir");
        let package = write_sample(temp.path());

        let manifest = read_manifest(&package).expect("read manifest");
        assert_eq!(manifest, sample_manifest());

        let out = temp.path().join("out.mp4");
        extract_file(&package, "wall.mp4", &out).expect("extract entry");
        assert_eq!(
            std::fs::read(&out).expect("read out"),
            b"not really a video"
        );
    }

    #[test]
    fn rejects_manifest_referencing_missing_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let media = temp.path().join("wall.mp4");
        std::fs::write(&media, b"x").expect("write media");
        let mut manifest = sample_manifest();
        manifest.preview = Some("missing.jpg".into());
        let package = temp.path().join("broken.wpk");
        let error = write_package(&package, &manifest, &[("wall.mp4", media.as_path())])
            .expect_err("must reject");
        assert!(matches!(error, WpkError::MissingFile(name) if name == "missing.jpg"));
    }

    #[test]
    fn rejects_empty_required_fields_and_unsafe_names() {
        let mut manifest = sample_manifest();
        manifest.author = "  ".into();
        assert!(matches!(
            validate_manifest(&manifest),
            Err(WpkError::InvalidManifest(_))
        ));

        for bad in ["../evil.mp4", "c:evil", "a\\b.mp4", "/abs.mp4", ""] {
            assert!(
                matches!(ensure_safe_name(bad), Err(WpkError::UnsafeName(_))),
                "should reject {bad:?}"
            );
        }
        assert!(ensure_safe_name("folder/wall.mp4").is_ok());
    }

    #[test]
    fn capped_readers_reject_oversized_streams() {
        // 100 bytes available, 64-byte cap -> rejected by actual length,
        // regardless of any declared size elsewhere.
        let data = vec![0u8; 100];
        let error = read_capped(&mut data.as_slice(), 64, "x").expect_err("must reject");
        assert!(matches!(error, WpkError::TooLarge { limit: 64, .. }));

        let mut sink = Vec::new();
        let error = copy_capped(&mut data.as_slice(), &mut sink, 64, "x").expect_err("must reject");
        assert!(matches!(error, WpkError::TooLarge { limit: 64, .. }));

        // Exactly at the cap is fine.
        let ok = vec![7u8; 64];
        assert_eq!(
            read_capped(&mut ok.as_slice(), 64, "x")
                .expect("fits")
                .len(),
            64
        );
        let mut sink = Vec::new();
        copy_capped(&mut ok.as_slice(), &mut sink, 64, "x").expect("fits");
        assert_eq!(sink.len(), 64);
    }

    #[test]
    fn rejects_archives_without_manifest() {
        let temp = tempfile::tempdir().expect("temp dir");
        let package = temp.path().join("empty.wpk");
        let mut writer = ZipWriter::new(File::create(&package).expect("create"));
        writer
            .start_file("something.txt", SimpleFileOptions::default())
            .expect("start");
        writer.write_all(b"hello").expect("write");
        writer.finish().expect("finish");

        assert!(matches!(
            read_manifest(&package),
            Err(WpkError::MissingManifest)
        ));
    }
}
