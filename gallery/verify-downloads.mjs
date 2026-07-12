// Downloads every pack in gallery/catalog.json and checks its SHA-256 and size
// against the catalog claim. Runs in CI on pull requests (see
// .github/workflows/gallery-validate.yml) so a catalog entry can't advertise a
// file it doesn't actually match. No-op on an empty catalog.

import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";

const catalog = JSON.parse(readFileSync(new URL("./catalog.json", import.meta.url), "utf8"));
const packs = Array.isArray(catalog.packs) ? catalog.packs : [];

let failed = false;
for (const pack of packs) {
  process.stdout.write(`• ${pack.id}: скачиваю… `);
  try {
    const response = await fetch(pack.download_url, { redirect: "follow" });
    if (!response.ok) {
      console.log(`✗ HTTP ${response.status}`);
      failed = true;
      continue;
    }
    const buffer = Buffer.from(await response.arrayBuffer());
    const sha = createHash("sha256").update(buffer).digest("hex");
    if (sha.toLowerCase() !== String(pack.sha256).toLowerCase()) {
      console.log(`✗ sha256 не совпал (файл: ${sha})`);
      failed = true;
    } else if (buffer.length !== pack.size) {
      console.log(`✗ size ${buffer.length} ≠ заявленного ${pack.size}`);
      failed = true;
    } else {
      console.log("✓");
    }
  } catch (error) {
    console.log("✗ " + error.message);
    failed = true;
  }
}

if (failed) {
  console.error("\nПроверка файлов не прошла.");
  process.exit(1);
} else {
  console.log(`Проверено файлов: ${packs.length}.`);
}
