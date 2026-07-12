//! Community gallery client: fetch the signed-later, HTTPS-served catalog from
//! the project's GitHub repo, download a pack, verify its SHA-256 against the
//! catalog entry, and hand it to the existing library import. v1 has no
//! signature (HTTPS + SHA-256 + PR moderation); offline signing is a later
//! hardening step (see docs/research/workshop.md).

use std::io::Read;

use serde::{Deserialize, Serialize};

use crate::library;

/// Where the catalog lives. Overridable for local testing.
const CATALOG_URL: &str =
    "https://raw.githubusercontent.com/sweety1lime/LimeWall-Gallery/master/gallery/catalog.json";
/// Catalogs above this are rejected as hostile.
const MAX_CATALOG_BYTES: u64 = 4 * 1024 * 1024;
/// Per-pack download cap (mirrors the web-folder limit in `library`).
const MAX_PACK_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GalleryPack {
    pub id: String,
    pub name: String,
    pub author: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub license: String,
    /// Lowercase hex SHA-256 of the `.wpk`.
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub preview: Option<String>,
    pub download_url: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Catalog {
    #[allow(dead_code)]
    version: u32,
    packs: Vec<GalleryPack>,
}

fn catalog_url() -> String {
    std::env::var("LIMEWALL_CATALOG_URL").unwrap_or_else(|_| CATALOG_URL.to_owned())
}

/// Only our own GitHub-hosted files may be fetched, so a tampered catalog can't
/// point downloads at an arbitrary host. GitHub release assets redirect through
/// `objects.githubusercontent.com`, which ureq follows.
fn is_allowed_url(url: &str) -> bool {
    const ALLOWED: [&str; 3] = [
        "https://raw.githubusercontent.com/",
        "https://github.com/",
        "https://objects.githubusercontent.com/",
    ];
    ALLOWED.iter().any(|prefix| url.starts_with(prefix))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn http_get_bytes(url: &str, max: u64) -> Result<Vec<u8>, String> {
    if !is_allowed_url(url) {
        return Err(format!("недопустимый адрес: {url}"));
    }
    let response = ureq::get(url)
        .call()
        .map_err(|error| format!("не удалось загрузить: {error}"))?;
    let mut reader = response.into_reader().take(max + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > max {
        return Err("файл слишком большой".to_owned());
    }
    Ok(bytes)
}

fn parse_catalog(body: &str) -> Result<Vec<GalleryPack>, String> {
    let catalog: Catalog =
        serde_json::from_str(body).map_err(|error| format!("некорректный каталог: {error}"))?;
    Ok(catalog.packs)
}

/// Downloads and parses the gallery catalog.
#[tauri::command]
pub async fn gallery_fetch_catalog() -> Result<Vec<GalleryPack>, String> {
    crate::blocking(|| {
        let bytes = http_get_bytes(&catalog_url(), MAX_CATALOG_BYTES)?;
        let body = String::from_utf8(bytes).map_err(|error| error.to_string())?;
        parse_catalog(&body)
    })
    .await
}

/// Downloads a pack, verifies its SHA-256 against the catalog entry, and imports
/// it into the library. The gallery is media-only (video/image) in v1, so this
/// never installs code without the consent flow.
#[tauri::command]
pub async fn gallery_download(pack: GalleryPack) -> Result<library::LibraryItem, String> {
    crate::blocking(move || {
        let bytes = http_get_bytes(&pack.download_url, MAX_PACK_BYTES)?;
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(pack.sha256.trim()) {
            return Err(format!(
                "контрольная сумма не совпала для «{}» — загрузка отклонена",
                pack.name
            ));
        }
        // Import goes through the same validated `.wpk` path as a local import.
        let temp =
            std::env::temp_dir().join(format!("limewall-gallery-{}.wpk", sanitize(&pack.id)));
        std::fs::write(&temp, &bytes).map_err(|error| error.to_string())?;
        let result = library::Library::default_location().and_then(|lib| lib.import(&temp));
        let _ = std::fs::remove_file(&temp);
        result
    })
    .await
}

/// A filesystem-safe temp name fragment from a pack id.
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        // SHA-256 of "abc".
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn only_github_urls_are_allowed() {
        assert!(is_allowed_url(
            "https://raw.githubusercontent.com/sweety1lime/LimeWall-Gallery/master/gallery/catalog.json"
        ));
        assert!(is_allowed_url(
            "https://github.com/x/y/releases/download/v1/a.wpk"
        ));
        assert!(!is_allowed_url("https://evil.example.com/a.wpk"));
        assert!(!is_allowed_url("http://github.com/x/y")); // not https
    }

    #[test]
    fn parses_a_catalog_and_defaults_optional_fields() {
        let json = r#"{
            "version": 1,
            "packs": [
                { "id": "aurora", "name": "Aurora", "author": "2fame", "type": "video",
                  "license": "CC-BY-4.0", "sha256": "AB", "size": 10,
                  "download_url": "https://github.com/x/y/releases/download/v1/a.wpk" }
            ]
        }"#;
        let packs = parse_catalog(json).expect("valid catalog");
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].id, "aurora");
        assert!(packs[0].preview.is_none());
        assert!(packs[0].tags.is_empty());
    }

    #[test]
    fn empty_catalog_parses() {
        assert!(
            parse_catalog(r#"{"version":1,"packs":[]}"#)
                .unwrap()
                .is_empty()
        );
    }
}
