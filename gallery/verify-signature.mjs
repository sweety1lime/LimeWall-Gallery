// Verifies gallery/catalog.json.sig against gallery/catalog.json with the
// committed public key. Runs in CI and locally. When signing is not enabled
// (empty pubkey) it exits 0 — nothing to check yet.

import { createPublicKey, verify } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";

const pubPath = new URL("./pubkey.ed25519", import.meta.url);
const catalogPath = new URL("./catalog.json", import.meta.url);
const sigPath = new URL("./catalog.json.sig", import.meta.url);

const keyLine = readFileSync(pubPath, "utf8")
  .split(/\r?\n/)
  .map((line) => line.trim())
  .find((line) => line && !line.startsWith("#"));

if (!keyLine) {
  console.log("✓ подпись каталога не включена — пропуск");
  process.exit(0);
}

const raw = Buffer.from(keyLine, "base64");
if (raw.length !== 32) {
  console.error("✗ pubkey.ed25519: ключ должен быть 32 байта (base64)");
  process.exit(1);
}
const publicKey = createPublicKey({
  key: { kty: "OKP", crv: "Ed25519", x: raw.toString("base64url") },
  format: "jwk",
});

if (!existsSync(sigPath)) {
  console.error("✗ каталог подписан, но нет catalog.json.sig — node gallery/sign-catalog.mjs");
  process.exit(1);
}

const data = readFileSync(catalogPath);
const signature = Buffer.from(readFileSync(sigPath, "utf8").trim(), "base64");
if (verify(null, data, publicKey, signature)) {
  console.log("✓ подпись каталога верна");
} else {
  console.error("✗ подпись каталога НЕ совпала — пересобери каталог и подпиши заново");
  process.exit(1);
}
