// Signs gallery/catalog.json with the offline private key, producing
// gallery/catalog.json.sig (base64 of the detached Ed25519 signature over the
// exact catalog bytes). Run after every `node gallery/build-catalog.mjs`.

import { createPrivateKey, sign } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";

const privPath = new URL("./private-key.ed25519", import.meta.url);
const catalogPath = new URL("./catalog.json", import.meta.url);
const sigPath = new URL("./catalog.json.sig", import.meta.url);

if (!existsSync(privPath)) {
  console.error("нет gallery/private-key.ed25519 — сначала node gallery/keygen.mjs");
  process.exit(1);
}

const key = createPrivateKey(readFileSync(privPath));
const data = readFileSync(catalogPath); // exact bytes — must match what the client fetches
const signature = sign(null, data, key); // ed25519: algorithm is null
writeFileSync(sigPath, `${signature.toString("base64")}\n`);

console.log("✓ подписано -> gallery/catalog.json.sig");
console.log("  закоммить catalog.json И catalog.json.sig вместе.");
