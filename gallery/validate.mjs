// Validates gallery/catalog.json for the LimeWall community gallery.
// Runs in CI on any PR touching gallery/ (see .github/workflows/gallery-validate.yml)
// and can be run locally: `node gallery/validate.mjs`.

import { readFileSync } from "node:fs";

const REQUIRED = ["id", "name", "author", "type", "license", "sha256", "size", "download_url"];
const ALLOWED_TYPES = ["video", "image"]; // v1: media only — code (web/3D) is not accepted yet
const HEX64 = /^[0-9a-fA-F]{64}$/;
const GITHUB_URL = /^https:\/\/(github\.com|raw\.githubusercontent\.com)\//;
const SLUG = /^[a-z0-9-]+$/;
const MAX_TEXT = 80;
// Packs are served from the repo, so keep them small to avoid bloating git.
const MAX_PACK_BYTES = 32 * 1024 * 1024;

function fail(message) {
  console.error("✗ " + message);
  process.exitCode = 1;
}

let catalog;
try {
  catalog = JSON.parse(readFileSync(new URL("./catalog.json", import.meta.url), "utf8"));
} catch (error) {
  fail("catalog.json не парсится: " + error.message);
  process.exit(1);
}

if (catalog.version !== 1) fail("version должно быть 1");
if (!Array.isArray(catalog.packs)) {
  fail("packs должен быть массивом");
  process.exit(1);
}

const ids = new Set();
for (const [i, pack] of catalog.packs.entries()) {
  const where = `pack #${i} (${pack.id ?? "?"})`;
  for (const key of REQUIRED) {
    if (pack[key] === undefined || pack[key] === "") fail(`${where}: нет поля ${key}`);
  }
  if (pack.type && !ALLOWED_TYPES.includes(pack.type)) {
    fail(`${where}: тип "${pack.type}" не разрешён — в v1 только video/image`);
  }
  if (pack.sha256 && !HEX64.test(pack.sha256)) {
    fail(`${where}: sha256 должен быть 64 hex-символа`);
  }
  if (pack.download_url && !GITHUB_URL.test(pack.download_url)) {
    fail(`${where}: download_url должен вести на github.com или raw.githubusercontent.com`);
  }
  if (pack.preview !== undefined && !GITHUB_URL.test(pack.preview)) {
    fail(`${where}: preview должен вести на github.com или raw.githubusercontent.com`);
  }
  if (typeof pack.size !== "number" || pack.size <= 0 || pack.size > MAX_PACK_BYTES) {
    fail(`${where}: size должен быть 1..${MAX_PACK_BYTES} байт`);
  }
  if (pack.id && !SLUG.test(pack.id)) {
    fail(`${where}: id должен быть slug (строчные латинские буквы, цифры, дефис)`);
  }
  for (const field of ["name", "author"]) {
    if (typeof pack[field] === "string" && pack[field].length > MAX_TEXT) {
      fail(`${where}: ${field} длиннее ${MAX_TEXT} символов`);
    }
  }
  if (pack.tags !== undefined && !Array.isArray(pack.tags)) {
    fail(`${where}: tags должен быть массивом`);
  }
  if (pack.id) {
    if (ids.has(pack.id)) fail(`${where}: повторяющийся id`);
    ids.add(pack.id);
  }
}

// --- revocation list -------------------------------------------------------
// A revoked pack must be *removed* from the repo (so the catalog no longer
// carries it) and its id recorded here, which pulls it from anyone who already
// installed it. So the invariant is: nothing in the catalog is also revoked.
let revocation;
try {
  revocation = JSON.parse(readFileSync(new URL("./revocation.json", import.meta.url), "utf8"));
} catch (error) {
  fail("revocation.json не парсится: " + error.message);
  revocation = { version: 1, revoked: [] };
}

if (revocation.version !== 1) fail("revocation.version должно быть 1");
if (!Array.isArray(revocation.revoked)) {
  fail("revocation.revoked должен быть массивом");
  revocation.revoked = [];
}

const revokedIds = new Set();
for (const [i, entry] of revocation.revoked.entries()) {
  const where = `revoked #${i} (${entry.id ?? "?"})`;
  if (!entry.id || !SLUG.test(entry.id)) fail(`${where}: id должен быть slug`);
  if (entry.sha256 !== undefined && !HEX64.test(entry.sha256)) {
    fail(`${where}: sha256 должен быть 64 hex-символа`);
  }
  if (entry.id) {
    if (ids.has(entry.id)) {
      fail(`${where}: пак ещё в каталоге — удалите gallery/packs/${entry.id}/ и пересоберите каталог`);
    }
    revokedIds.add(entry.id);
  }
}

if (process.exitCode) {
  console.error("\nВалидация каталога не прошла.");
} else {
  console.log(
    `✓ Каталог валиден: ${catalog.packs.length} пак(ов), ${revokedIds.size} отозвано.`,
  );
}
