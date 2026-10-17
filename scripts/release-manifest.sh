#!/bin/sh
# Write and sign the update manifest for a release.
#
#   VOID_RELEASE_SIGNING_KEY="$(cat key.pem)" scripts/release-manifest.sh <version> <assets-dir>
#
# Writes into <assets-dir>:
#   manifest.json      the version and every asset's name, size and SHA-256
#   manifest.json.sig  Ed25519 signature over manifest.json, hex
#   SHA256SUMS         the same hashes in `shasum -c` format, for people
#
# The app checks manifest.json.sig against the public key it embeds
# (crates/deepclean-core/src/update/release-key.pem) before trusting any hash
# in the manifest, and refuses a manifest whose version is not the release it
# is installing. Needs OpenSSL 3 and python3.
set -eu

VERSION="$1"
DIR="$2"
PUBKEY="$(cd "$(dirname "$0")/.." && pwd)/crates/deepclean-core/src/update/release-key.pem"

if [ -z "${VOID_RELEASE_SIGNING_KEY:-}" ]; then
  echo "error: VOID_RELEASE_SIGNING_KEY is not set; add the release signing key as a repository secret" >&2
  exit 1
fi

cd "$DIR"
rm -f manifest.json manifest.json.sig SHA256SUMS

python3 - "$VERSION" <<'EOF'
import hashlib, json, os, sys

version = sys.argv[1]
assets = []
for name in sorted(os.listdir(".")):
    if not os.path.isfile(name) or name.startswith("."):
        continue
    h = hashlib.sha256()
    with open(name, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    assets.append({"name": name, "size": os.path.getsize(name), "sha256": h.hexdigest()})
if not assets:
    sys.exit("error: no release assets to list")
with open("manifest.json", "w") as f:
    json.dump({"version": version, "assets": assets}, f, indent=2)
    f.write("\n")
with open("SHA256SUMS", "w") as f:
    for a in assets:
        f.write(f"{a['sha256']}  {a['name']}\n")
EOF

KEY="$(mktemp)"
SIG="$(mktemp)"
trap 'rm -f "$KEY" "$SIG"' EXIT
chmod 600 "$KEY"
printf '%s\n' "$VOID_RELEASE_SIGNING_KEY" > "$KEY"

openssl pkeyutl -sign -inkey "$KEY" -rawin -in manifest.json -out "$SIG"
# Refuse to publish a signature the app would reject: the secret must match
# the committed public key.
if ! openssl pkeyutl -verify -pubin -inkey "$PUBKEY" -rawin -in manifest.json -sigfile "$SIG" >/dev/null; then
  echo "error: the signing key does not match $PUBKEY" >&2
  exit 1
fi
od -An -v -tx1 "$SIG" | tr -d ' \n' > manifest.json.sig
echo >> manifest.json.sig

echo "manifest.json: $(python3 -c 'import json; print(len(json.load(open("manifest.json"))["assets"]))') assets, signed"
