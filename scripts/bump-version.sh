#!/usr/bin/env bash
# TB: bump the FOUR version pins together — Cargo.toml, gui/Cargo.toml, Packager.toml,
# packaging/macos/Info.plist (CFBundleVersion) — plus the Cargo.lock refresh, in one command. The
# pins are hand-synced by design (no workspace inheritance) and THREE separate CI checks fail on
# drift, so the bump has to be atomic; every release commit before this script did it by hand
# ("bump the four version pins together"). The self-updater raised the stakes: fab-gui's baked
# CARGO_PKG_VERSION is now what update checks compare, so a missed pin isn't cosmetic anymore.
#
# usage: scripts/bump-version.sh <new-version>   # bare semver, e.g. 1.4.0
set -euo pipefail
cd "$(dirname "$0")/.."

NEW="${1:?usage: bump-version.sh <new-version> (bare semver, no leading v)}"
[[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "not a bare semver: $NEW" >&2; exit 1; }
OLD=$(sed -n 's/^version = "\(.*\)"/\1/p' Packager.toml)

# Each file has exactly ONE line-start `version = "` (checked when this script landed); a second
# one would silently double-edit, so assert before touching anything.
for f in Cargo.toml gui/Cargo.toml Packager.toml; do
  [[ "$(grep -c '^version = ' "$f")" == 1 ]] || { echo "$f grew a second ^version line — fix the script first" >&2; exit 1; }
done

for f in Cargo.toml gui/Cargo.toml Packager.toml; do
  sed -i '' "s/^version = \".*\"/version = \"$NEW\"/" "$f"
done
# sed, NOT `PlistBuddy Set`: PlistBuddy REWRITES the plist and strips the W.2.2.1 comment that
# explains why the pin exists (the file itself warns about this; the first run of this script
# proved it). The pin dict holds exactly one <string> — assert that before touching it.
PLIST=packaging/macos/Info.plist
[[ "$(grep -c '<string>' "$PLIST")" == 1 ]] || { echo "$PLIST grew a second <string> — fix the script first" >&2; exit 1; }
sed -i '' "s|<string>.*</string>|<string>$NEW</string>|" "$PLIST"
# Refresh the two members' versions in Cargo.lock (metadata resolves + rewrites a stale lock).
cargo metadata --format-version 1 >/dev/null

echo "bumped $OLD -> $NEW:"
grep -Hn '^version = ' Cargo.toml gui/Cargo.toml Packager.toml
echo "Info.plist CFBundleVersion = $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' packaging/macos/Info.plist)"
echo "next: commit, wait for green CI, then: git tag v$NEW && git push origin v$NEW"
