#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source_bundle=$("$root/scripts/build-app.sh" | tail -1)
install_root="$HOME/Applications"
installed_bundle="$install_root/Spaceman.app"

mkdir -p "$install_root"
pkill -x spaceman-agent >/dev/null 2>&1 || true
for _ in 1 2 3 4 5 6 7 8 9 10; do
  pgrep -x spaceman-agent >/dev/null 2>&1 || break
  sleep 0.1
done
if pgrep -x spaceman-agent >/dev/null 2>&1; then
  echo "Spaceman did not stop before installation" >&2
  exit 1
fi
rm -rf "$installed_bundle"
ditto "$source_bundle" "$installed_bundle"
codesign --verify --deep --strict "$installed_bundle"
open "$installed_bundle"

printf '%s\n' "$installed_bundle"
