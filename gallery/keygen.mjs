// Generates the Ed25519 keypair that signs the gallery catalog. Run ONCE.
//
//   PRIVATE key -> gallery/private-key.ed25519  (gitignored — keep it OFFLINE,
//                  it is the whole trust anchor; losing it means shipping a new
//                  app build with a new public key).
//   PUBLIC key  -> gallery/pubkey.ed25519  (committed; compiled into the client).
//
// After running: rebuild/ship the app (the public key is baked in) and sign the
// catalog with `node gallery/sign-catalog.mjs`.

import { generateKeyPairSync } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";

const privPath = new URL("./private-key.ed25519", import.meta.url);
const pubPath = new URL("./pubkey.ed25519", import.meta.url);
const force = process.argv.includes("--force");

if (existsSync(privPath) && !force) {
  console.error(
    "gallery/private-key.ed25519 уже существует. Ротация ключа требует новой сборки\n" +
      "приложения с новым публичным ключом. Перезаписать намеренно: --force",
  );
  process.exit(1);
}

const { publicKey, privateKey } = generateKeyPairSync("ed25519");

// Raw 32-byte public key, base64. (JWK `x` is base64url of exactly those bytes.)
const rawPub = Buffer.from(publicKey.export({ format: "jwk" }).x, "base64url");
const pem = privateKey.export({ type: "pkcs8", format: "pem" });

writeFileSync(privPath, pem, { mode: 0o600 });

// Keep the explanatory header, replace/append the single key line.
const header = readFileSync(pubPath, "utf8")
  .split(/\r?\n/)
  .filter((line) => line.startsWith("#") || line.trim() === "")
  .join("\n")
  .trimEnd();
writeFileSync(pubPath, `${header}\n${rawPub.toString("base64")}\n`);

console.log("✓ ключи созданы:");
console.log("  публичный  -> gallery/pubkey.ed25519  (закоммитить, вшивается в клиент)");
console.log("  приватный  -> gallery/private-key.ed25519  (НЕ коммитить, хранить офлайн)");
console.log("\nДальше: пересобрать/выпустить приложение, затем node gallery/sign-catalog.mjs");
