#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source_bundle=$("$root/scripts/build-app.sh")
install_root="/Applications"
installed_bundle="$install_root/Spaceman.app"
cli_root="$HOME/.local/bin"
was_automatic=false

mkdir -p "$install_root"
if pgrep -x spaceman-agent >/dev/null 2>&1; then
  status=$("$source_bundle/Contents/MacOS/spaceman" ctl status 2>/dev/null || true)
  case "$status" in
    *'"mode":"automatic"'*) was_automatic=true ;;
  esac
  if ! "$source_bundle/Contents/MacOS/spaceman" ctl restart >/dev/null 2>&1; then
    pkill -TERM -x spaceman-agent
  fi
fi
for _ in $(seq 1 75); do
  pgrep -x spaceman-agent >/dev/null 2>&1 || break
  sleep 0.2
done
if pgrep -x spaceman-agent >/dev/null 2>&1; then
  echo "Spaceman did not finish restoring windows; installation aborted" >&2
  exit 1
fi
rm -rf "$installed_bundle"
ditto "$source_bundle" "$installed_bundle"
codesign --verify --deep --strict "$installed_bundle"
mkdir -p "$cli_root"
ln -sf "$installed_bundle/Contents/MacOS/spaceman" "$cli_root/spaceman"
for _ in $(seq 1 10); do
  open "$installed_bundle" 2>/dev/null && break
  sleep 0.2
done
if $was_automatic; then
  for _ in $(seq 1 75); do
    if "$installed_bundle/Contents/MacOS/spaceman" ctl resume >/dev/null 2>&1; then
      break
    fi
    sleep 0.2
  done
fi

printf '%s\n' "$installed_bundle"
