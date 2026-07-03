#!/usr/bin/env bash
# Local fab-web loop: build -> bindgen -> stage the contract dir -> serve with prod headers.
# The release pipeline (release-web.yml) is for SHIPPING; this is the seconds-scale iterate loop.
#
#   ./packaging/web/dev.sh               # build + stage + serve http://127.0.0.1:8787/
#   ./packaging/web/dev.sh --stage-only  # refresh target/fab-web/stage and exit — the dir a
#                                        # hotchkiss-io local override consumes instead of the
#                                        # GitHub release (see docs/web-bundle.md)
#   PORT=9000 ./packaging/web/dev.sh     # different port
#
# Differences vs the released artifact, on purpose: no wasm-opt (slow, size-only) and fast
# compression levels — the file SET still matches the contract so consumers don't special-case.
set -euo pipefail
cd "$(dirname "$0")/../.."

CRATE=${FAB_WEB_CRATE:-spikes/wasm-gui} # flips to the real fab-web crate at build-out
(cd "$CRATE" && cargo build --release --target wasm32-unknown-unknown)
WASM=$(ls "$CRATE"/target/wasm32-unknown-unknown/release/*.wasm | head -1)

STAGE=target/fab-web/stage
mkdir -p "$STAGE"
wasm-bindgen --target web --no-typescript --out-name fab_web --out-dir "$STAGE" "$WASM"
cp packaging/web/index.reference.html "$STAGE/index.html"
# Contract-complete variants at dev-grade compression (brotli -q5 ~seconds vs -q11 ~minutes).
if command -v brotli >/dev/null; then brotli -q 5 -kf "$STAGE/fab_web_bg.wasm"; fi
gzip -6 -kf "$STAGE/fab_web_bg.wasm"
python3 - "$STAGE" <<'EOF'
import hashlib, json, os, sys
stage = sys.argv[1]
sha = {f: hashlib.sha256(open(os.path.join(stage, f), 'rb').read()).hexdigest()
       for f in sorted(os.listdir(stage)) if f not in ("manifest.json", "index.html")}
json.dump({"version": "dev", "entry": "fab_web.js", "wasm": "fab_web_bg.wasm", "sha256": sha},
          open(os.path.join(stage, "manifest.json"), "w"), indent=1)
EOF
echo "staged -> $STAGE"

[ "${1:-}" = "--stage-only" ] && exit 0
exec python3 packaging/web/dev-server.py "$STAGE" "${PORT:-8787}"
