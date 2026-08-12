#!/usr/bin/env bash
# TB: author the update manifest (`latest.json`) the in-app updater polls. cargo-packager emits the
# signed `.app.tar.gz` but NOT the manifest (upstream issue #350), so the release workflow writes it
# with this script — and the e2e test (gui/tests/update_e2e.rs) calls the SAME script, so the shape
# CI publishes and the shape the updater parses can't drift apart.
#
# The platform key is `macos-aarch64` and that is not a typo for `darwin-aarch64`: the updater's own
# 0.2.3 docs show darwin-* keys its code can never match (get_updater_target() returns "macos");
# copy the docs and updates silently never fire. `format: "app"` is REQUIRED — a platform entry
# without it fails deserialization of the ENTIRE manifest. `pub_date` must be strict RFC 3339.
#
# usage: make-update-manifest.sh <version> <url> <sig-file> <out-file> [notes]
set -euo pipefail

[[ $# -ge 4 ]] || { echo "usage: $0 <version> <url> <sig-file> <out-file> [notes]" >&2; exit 2; }
VERSION="$1" URL="$2" SIG_FILE="$3" OUT="$4" NOTES="${5:-}"
[[ -f "$SIG_FILE" ]] || { echo "no signature file at $SIG_FILE — was CARGO_PACKAGER_SIGN_PRIVATE_KEY set when cargo-packager ran?" >&2; exit 1; }

# python3 (present on macOS + the runners) so the signature lands JSON-escaped, not sed-mangled.
VERSION="$VERSION" URL="$URL" SIG_FILE="$SIG_FILE" OUT="$OUT" NOTES="$NOTES" python3 - <<'EOF'
import json, os, time

manifest = {
    "version": os.environ["VERSION"],
    "pub_date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "platforms": {
        "macos-aarch64": {
            "url": os.environ["URL"],
            "signature": open(os.environ["SIG_FILE"]).read().strip(),
            "format": "app",
        }
    },
}
if os.environ["NOTES"]:
    manifest["notes"] = os.environ["NOTES"]
with open(os.environ["OUT"], "w") as f:
    json.dump(manifest, f, indent=2)
print(f"wrote {os.environ['OUT']} for {os.environ['VERSION']}")
EOF
