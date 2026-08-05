#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bundle="$root/target/Spaceman.app"
identity=${SPACEMAN_CODESIGN_IDENTITY:-}
deployment_target=$(/usr/bin/plutil -extract LSMinimumSystemVersion raw "$root/app/Info.plist")

if [ -z "$identity" ]; then
  identity=$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p' \
    | head -1)
fi
if [ -z "$identity" ]; then
  echo "No Developer ID Application identity found; set SPACEMAN_CODESIGN_IDENTITY." >&2
  exit 1
fi

MACOSX_DEPLOYMENT_TARGET="$deployment_target" \
  CARGO_TARGET_DIR="$root/target" \
  cargo build --manifest-path "$root/Cargo.toml" --release --locked --bins
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS"
cp "$root/target/release/spaceman-agent" "$bundle/Contents/MacOS/spaceman-agent"
cp "$root/target/release/spaceman" "$bundle/Contents/MacOS/spaceman"
cp "$root/app/Info.plist" "$bundle/Contents/Info.plist"
plutil -lint "$bundle/Contents/Info.plist" >/dev/null
codesign --force --timestamp --options runtime --sign "$identity" \
  "$bundle/Contents/MacOS/spaceman-agent"
codesign --force --timestamp --options runtime --sign "$identity" \
  "$bundle/Contents/MacOS/spaceman"
codesign --force --timestamp --options runtime --sign "$identity" \
  --identifier com.kitlangton.spaceman \
  "$bundle"
codesign --verify --deep --strict "$bundle"

printf '%s\n' "$bundle"
