// Validates gallery/catalog.json for the LimeWall community gallery.
// Runs in CI on any PR touching gallery/ (see .github/workflows/gallery-validate.yml)
// and can be run locally: `node gallery/validate.mjs`.

import { readFileSync } from "node:fs";

const REQUIRED = ["id", "name", "author", "type", "license", "sha256", "size", "download_url"];
const ALLOWED_TYPES = ["video", "image"]; // v1: media only — code (web/3D) is not accepted yet
const HEX64 = /^[0-9a-fA-F]{64}$/;
const GITHUB_URL = /^https:\/\/(github\.com|raw\.githubusercontent\.com)\//;

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
  if (typeof pack.size !== "number" || pack.size <= 0) {
    fail(`${where}: size должен быть положительным числом байт`);
  }
  if (pack.id) {
    if (ids.has(pack.id)) fail(`${where}: повторяющийся id`);
    ids.add(pack.id);
  }
}

if (process.exitCode) {
  console.error("\nВалидация каталога не прошла.");
} else {
  console.log(`✓ Каталог валиден: ${catalog.packs.length} пак(ов).`);
}
