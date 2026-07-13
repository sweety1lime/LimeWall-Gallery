//! Community gallery client: fetch the signed-later, HTTPS-served catalog from
//! the project's GitHub repo, download a pack, verify its SHA-256 against the
//! catalog entry, and hand it to the existing library import. v1 has no
//! signature (HTTPS + SHA-256 + PR moderation); offline signing is a later
//! hardening step (see docs/research/workshop.md).
//!
//! A published pack can also be **revoked**: `revocation.json` lists ids (and
//! optional file hashes) that must never be installed and should be pulled from
//! anyone who already has them. The client filters revoked packs out of the
//! catalog, refuses to download them, and sweeps them out of the library. This
//! is a best-effort kill-switch — if the revocation list is unreachable we fail
//! open (the gallery keeps working) rather than blocking the whole app.

use std::collections::HashSet;
use std::io::Read;

use serde::{Deserialize, Serialize};

use crate::library;

/// Where the catalog lives. Overridable for local testing.
const CATALOG_URL: &str =
    "https://raw.githubusercontent.com/sweety1lime/LimeWall-Gallery/master/gallery/catalog.json";
/// Where the revocation list lives. Overridable for local testing.
const REVOCATION_URL: &str =
    "https://raw.githubusercontent.com/sweety1lime/LimeWall-Gallery/master/gallery/revocation.json";
/// Detached Ed25519 signature over the exact bytes of `catalog.json`.
const SIGNATURE_URL: &str =
    "https://raw.githubusercontent.com/sweety1lime/LimeWall-Gallery/master/gallery/catalog.json.sig";
/// Catalogs above this are rejected as hostile.
const MAX_CATALOG_BYTES: u64 = 4 * 1024 * 1024;
/// The revocation list is tiny; anything larger is bogus.
const MAX_REVOCATION_BYTES: u64 = 1024 * 1024;
/// An Ed25519 signature is 64 bytes; base64 with slack.
const MAX_SIGNATURE_BYTES: u64 = 4096;
/// Per-pack download cap (mirrors the web-folder limit in `library`).
const MAX_PACK_BYTES: u64 = 512 * 1024 * 1024;

/// Ed25519 public key that signs the catalog, **compiled into** the client.
/// Empty (comments only) until `gallery/keygen.mjs` is run — then catalog
/// verification is enforced. Baking the key in rather than fetching it is the
/// point: a repository compromise cannot swap the key already shipped in a
/// binary, so a forged catalog is rejected on machines built before the breach.
const PUBKEY_FILE: &str = include_str!("../../../../gallery/pubkey.ed25519");

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

/// One revoked pack. `id` matches the library item id (and the catalog id);
/// `sha256`, when present, additionally blocks any `.wpk` with that file hash.
#[derive(Debug, Clone, Deserialize)]
struct RevocationEntry {
    id: String,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Revocation {
    #[serde(default)]
    revoked: Vec<RevocationEntry>,
}

/// The catalog the panel shows, plus any library items a revocation removed on
/// this fetch (so the UI can tell the user what was pulled).
#[derive(Debug, Default, Serialize)]
pub struct CatalogView {
    packs: Vec<GalleryPack>,
    removed: Vec<String>,
}

fn catalog_url() -> String {
    std::env::var("LIMEWALL_CATALOG_URL").unwrap_or_else(|_| CATALOG_URL.to_owned())
}

fn revocation_url() -> String {
    std::env::var("LIMEWALL_REVOCATION_URL").unwrap_or_else(|_| REVOCATION_URL.to_owned())
}

fn signature_url() -> String {
    std::env::var("LIMEWALL_CATALOG_SIG_URL").unwrap_or_else(|_| SIGNATURE_URL.to_owned())
}

/// The configured signing key, or `None` when signing is not enabled. The file
/// holds `#` comments and blank lines plus, once enabled, one base64 line of the
/// 32-byte Ed25519 public key.
fn parse_pubkey(content: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(line).ok()?;
    (bytes.len() == 32).then_some(bytes)
}

/// Verifies an Ed25519 signature over `message`. Uses ring, already in the tree
/// via rustls, so no new crypto dependency is added.
fn verify_ed25519(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
        .verify(message, signature)
        .is_ok()
}

/// Enforces the catalog signature. When signing is not enabled (empty key) this
/// is a no-op and the client keeps trusting HTTPS + per-pack SHA-256. When it is
/// enabled, a missing or invalid signature rejects the whole catalog — a
/// repo-compromised or tampered catalog cannot be served to an updated client.
fn verify_catalog_signature(catalog: &[u8]) -> Result<(), String> {
    use base64::Engine;
    let Some(public_key) = parse_pubkey(PUBKEY_FILE) else {
        return Ok(()); // signing not enabled yet
    };
    let signature = http_get_bytes(&signature_url(), MAX_SIGNATURE_BYTES)
        .map_err(|_| "каталог подписан, но подпись недоступна — отклонено".to_owned())?;
    let signature = String::from_utf8(signature)
        .ok()
        .and_then(|text| {
            base64::engine::general_purpose::STANDARD
                .decode(text.trim())
                .ok()
        })
        .ok_or("подпись каталога повреждена — отклонено")?;
    if verify_ed25519(&public_key, catalog, &signature) {
        Ok(())
    } else {
        Err("подпись каталога недействительна — отклонено".to_owned())
    }
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

fn parse_revocation(body: &str) -> Result<Revocation, String> {
    serde_json::from_str(body).map_err(|error| format!("некорректный список отзыва: {error}"))
}

/// Fetches the revocation list, failing open: any network/parse error yields an
/// empty list so a hiccup never blocks the gallery. The trade-off (an attacker
/// who can suppress this file delays revocation) is inherent to online revocation
/// and documented in docs/research/workshop.md.
fn fetch_revocation() -> Revocation {
    match http_get_bytes(&revocation_url(), MAX_REVOCATION_BYTES) {
        Ok(bytes) => String::from_utf8(bytes)
            .map_err(|error| error.to_string())
            .and_then(|body| parse_revocation(&body))
            .unwrap_or_default(),
        Err(_) => Revocation::default(),
    }
}

/// Ids that must not be installed. Both the pack id and any listed file hash are
/// treated as ids so a revoked-by-hash pack still matches a catalog entry.
fn revoked_ids(revocation: &Revocation) -> HashSet<String> {
    revocation
        .revoked
        .iter()
        .map(|entry| entry.id.to_ascii_lowercase())
        .collect()
}

/// Lowercase file hashes flagged for revocation.
fn revoked_hashes(revocation: &Revocation) -> HashSet<String> {
    revocation
        .revoked
        .iter()
        .filter_map(|entry| entry.sha256.as_deref())
        .map(|hash| hash.trim().to_ascii_lowercase())
        .collect()
}

/// Drops any catalog pack whose id or file hash has been revoked.
fn filter_packs(packs: Vec<GalleryPack>, revocation: &Revocation) -> Vec<GalleryPack> {
    let ids = revoked_ids(revocation);
    let hashes = revoked_hashes(revocation);
    packs
        .into_iter()
        .filter(|pack| {
            !ids.contains(&pack.id.to_ascii_lowercase())
                && !hashes.contains(&pack.sha256.trim().to_ascii_lowercase())
        })
        .collect()
}

/// True if this pack is revoked (used to refuse a download of a stale entry).
fn is_revoked(pack: &GalleryPack, revocation: &Revocation) -> bool {
    revoked_ids(revocation).contains(&pack.id.to_ascii_lowercase())
        || revoked_hashes(revocation).contains(&pack.sha256.trim().to_ascii_lowercase())
}

/// Removes any library item whose id was revoked and returns the removed items
/// (so their names can be shown and their now-playing sessions stopped). Only
/// gallery packs carry slug ids that match a revocation entry, so a user's own
/// content-hashed imports are never touched.
fn sweep_library(revocation: &Revocation) -> Result<Vec<library::LibraryItem>, String> {
    let ids = revoked_ids(revocation);
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let library = library::Library::default_location()?;
    let mut removed = Vec::new();
    for item in library.list()? {
        if ids.contains(&item.id.to_ascii_lowercase()) {
            match library.remove(&item.id) {
                Ok(()) => removed.push(item),
                Err(error) => eprintln!("revocation: could not remove {}: {error}", item.id),
            }
        }
    }
    Ok(removed)
}

/// Applies a revocation: sweeps the library and stops any wallpaper still
/// playing a file that was just removed. Returns the removed items' names.
fn apply_revocation(revocation: &Revocation) -> Result<Vec<String>, String> {
    let removed = sweep_library(revocation)?;
    let paths: Vec<_> = removed.iter().map(|item| item.file.clone()).collect();
    crate::daemon_client::stop_sessions_playing(&paths);
    Ok(removed.into_iter().map(|item| item.name).collect())
}

/// Downloads and parses the gallery catalog, applying the revocation list:
/// revoked packs are dropped from the returned catalog and swept out of the
/// library. `removed` names anything a revocation just pulled locally.
#[tauri::command]
pub async fn gallery_fetch_catalog() -> Result<CatalogView, String> {
    crate::blocking(|| {
        let bytes = http_get_bytes(&catalog_url(), MAX_CATALOG_BYTES)?;
        // Verify the signature over the exact fetched bytes before parsing.
        verify_catalog_signature(&bytes)?;
        let body = String::from_utf8(bytes).map_err(|error| error.to_string())?;
        let packs = parse_catalog(&body)?;
        let revocation = fetch_revocation();
        let removed = apply_revocation(&revocation).unwrap_or_default();
        Ok(CatalogView {
            packs: filter_packs(packs, &revocation),
            removed,
        })
    })
    .await
}

/// Fetches the revocation list and pulls any revoked wallpaper out of the
/// library, returning the removed items' names. Run at startup so the
/// kill-switch works even if the user never opens the gallery.
#[tauri::command]
pub async fn gallery_apply_revocations() -> Result<Vec<String>, String> {
    crate::blocking(|| apply_revocation(&fetch_revocation())).await
}

/// Downloads a pack, verifies its SHA-256 against the catalog entry, and imports
/// it into the library. The gallery is media-only (video/image) in v1, so this
/// never installs code without the consent flow.
#[tauri::command]
pub async fn gallery_download(pack: GalleryPack) -> Result<library::LibraryItem, String> {
    crate::blocking(move || {
        // A cached catalog on the panel could still offer a since-revoked pack;
        // re-check before installing.
        if is_revoked(&pack, &fetch_revocation()) {
            return Err(format!("«{}» отозван и не может быть установлен", pack.name));
        }
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

    fn pack(id: &str, sha: &str) -> GalleryPack {
        GalleryPack {
            id: id.into(),
            name: id.into(),
            author: "2fame".into(),
            kind: "video".into(),
            license: "CC0-1.0".into(),
            sha256: sha.into(),
            size: 10,
            preview: None,
            download_url: "https://github.com/x/y/a.wpk".into(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn revocation_parses_and_defaults_to_empty() {
        assert!(parse_revocation(r#"{"version":1,"revoked":[]}"#).unwrap().revoked.is_empty());
        // A missing `revoked` array (or an unknown extra field) still parses.
        assert!(parse_revocation(r#"{"version":1}"#).unwrap().revoked.is_empty());
    }

    #[test]
    fn filter_drops_revoked_by_id_and_hash_keeps_others() {
        let packs = vec![pack("good", "AA"), pack("bad-id", "BB"), pack("bad-hash", "CC")];
        let revocation = parse_revocation(
            r#"{"version":1,"revoked":[
                {"id":"bad-id"},
                {"id":"other","sha256":"cc"}
            ]}"#,
        )
        .unwrap();
        let kept = filter_packs(packs, &revocation);
        let ids: Vec<_> = kept.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["good"]); // revoked by id and by (case-insensitive) hash removed
    }

    #[test]
    fn is_revoked_matches_id_case_insensitively() {
        let revocation =
            parse_revocation(r#"{"version":1,"revoked":[{"id":"Bad-Pack"}]}"#).unwrap();
        assert!(is_revoked(&pack("bad-pack", "AA"), &revocation));
        assert!(!is_revoked(&pack("good", "AA"), &revocation));
    }

    #[test]
    fn no_revocations_means_no_ids() {
        assert!(revoked_ids(&Revocation::default()).is_empty());
        assert!(revoked_hashes(&Revocation::default()).is_empty());
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn verify_ed25519_accepts_rfc8032_vector_and_rejects_tampering() {
        // RFC 8032 section 7.1, test vector 1 (empty message).
        let public_key =
            hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let signature = hex(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        );
        assert!(verify_ed25519(&public_key, b"", &signature));

        let mut tampered = signature.clone();
        tampered[0] ^= 1;
        assert!(!verify_ed25519(&public_key, b"", &tampered));
        assert!(!verify_ed25519(&public_key, b"different", &signature));
    }

    #[test]
    fn parse_pubkey_ignores_comments_and_requires_32_bytes() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        assert!(parse_pubkey("").is_none());
        assert!(parse_pubkey("# only a comment\n\n").is_none());
        assert!(parse_pubkey("not valid base64 !!").is_none());
        // Wrong length is rejected.
        assert!(parse_pubkey(&b64.encode([0u8; 16])).is_none());
        // A real 32-byte key after a comment parses back to its bytes.
        let key = hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let file = format!("# LimeWall gallery key\n{}\n", b64.encode(&key));
        assert_eq!(parse_pubkey(&file), Some(key));
    }

    #[test]
    fn signing_is_disabled_by_default() {
        // The committed key file ships empty, so the shipped client enforces
        // nothing extra until keygen is run.
        assert!(parse_pubkey(PUBKEY_FILE).is_none());
    }
}
