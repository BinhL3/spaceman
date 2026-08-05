#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
plist="$root/app/Info.plist"
expected_bundle_id=com.kitlangton.spaceman
expected_executables="spaceman spaceman-agent"

fail() {
  echo "release validation failed: $*" >&2
  exit 1
}

plist_value() {
  /usr/bin/plutil -extract "$1" raw "$2"
}

cargo_version=$(awk -F ' *= *' '
  /^\[package\]$/ { package = 1; next }
  /^\[/ { package = 0 }
  package && $1 == "version" { gsub(/"/, "", $2); print $2; exit }
' "$root/Cargo.toml")
plist_version=$(plist_value CFBundleShortVersionString "$plist")
plist_build=$(plist_value CFBundleVersion "$plist")
bundle_id=$(plist_value CFBundleIdentifier "$plist")
deployment_target=$(plist_value LSMinimumSystemVersion "$plist")

[ -n "$cargo_version" ] || fail "Cargo package version is missing"
[ "$cargo_version" = "$plist_version" ] || \
  fail "Cargo version $cargo_version does not match Info.plist version $plist_version"
[ "$bundle_id" = "$expected_bundle_id" ] || \
  fail "unexpected bundle identifier $bundle_id (expected $expected_bundle_id)"
case "$plist_build" in
  ''|*[!0-9]*) fail "CFBundleVersion must be a positive integer" ;;
  0) fail "CFBundleVersion must be greater than zero" ;;
esac
[ -n "$deployment_target" ] || fail "Info.plist does not declare LSMinimumSystemVersion"
/usr/bin/plutil -lint "$plist" >/dev/null || fail "source Info.plist is invalid"

if [ "${1:-}" = "--metadata-only" ]; then
  printf 'Metadata valid: version %s, bundle ID %s, declared minimum macOS %s\n' \
    "$cargo_version" "$bundle_id" "$deployment_target"
  exit 0
fi

[ "$#" -eq 2 ] || {
  echo "usage: $0 --metadata-only" >&2
  echo "       $0 /path/to/Spaceman.app /path/to/Spaceman.zip" >&2
  exit 2
}

bundle=$1
archive=$2
expected_arch=${SPACEMAN_RELEASE_ARCH:-$(uname -m)}
case "$expected_arch" in
  arm64|x86_64) ;;
  *) fail "unsupported expected architecture '$expected_arch'; set SPACEMAN_RELEASE_ARCH to arm64 or x86_64" ;;
esac

[ -d "$bundle" ] || fail "app bundle not found: $bundle"
[ -f "$archive" ] || fail "ZIP artifact not found: $archive"
[ "$(plist_value CFBundleShortVersionString "$bundle/Contents/Info.plist")" = "$cargo_version" ] || \
  fail "built bundle version does not match Cargo"
[ "$(plist_value CFBundleIdentifier "$bundle/Contents/Info.plist")" = "$expected_bundle_id" ] || \
  fail "built bundle identifier is incorrect"
[ "$(plist_value LSMinimumSystemVersion "$bundle/Contents/Info.plist")" = "$deployment_target" ] || \
  fail "built bundle deployment target differs from source Info.plist"
[ "$(plist_value CFBundleExecutable "$bundle/Contents/Info.plist")" = "spaceman-agent" ] || \
  fail "built bundle has an unexpected main executable"

for executable in $expected_executables; do
  path="$bundle/Contents/MacOS/$executable"
  [ -f "$path" ] && [ -x "$path" ] || fail "missing executable: $executable"
  archs=$(/usr/bin/lipo -archs "$path")
  [ "$archs" = "$expected_arch" ] || \
    fail "$executable architecture is '$archs' (expected '$expected_arch')"
  binary_target=$(/usr/bin/vtool -show-build "$path" | awk '/minos/ { print $2; exit }')
  [ "$binary_target" = "$deployment_target" ] || \
    fail "$executable deployment target is $binary_target (Info.plist declares $deployment_target)"

  signature=$(/usr/bin/codesign --display --verbose=4 "$path" 2>&1) || \
    fail "$executable does not have a readable signature"
  printf '%s\n' "$signature" | grep -q '^Authority=Developer ID Application:' || \
    fail "$executable is not signed with a Developer ID Application identity"
  printf '%s\n' "$signature" | grep -q 'flags=.*runtime' || \
    fail "$executable signature does not enable the hardened runtime"
  printf '%s\n' "$signature" | grep -q '^Timestamp=' || \
    fail "$executable signature has no trusted timestamp"
  /usr/bin/codesign --verify --strict --verbose=2 "$path" || \
    fail "$executable signature verification failed"
done

bundle_signature=$(/usr/bin/codesign --display --verbose=4 "$bundle" 2>&1) || \
  fail "bundle does not have a readable signature"
printf '%s\n' "$bundle_signature" | grep -q '^Authority=Developer ID Application:' || \
  fail "bundle is not signed with a Developer ID Application identity"
printf '%s\n' "$bundle_signature" | grep -q "^Identifier=$expected_bundle_id$" || \
  fail "signed bundle identifier is incorrect"
printf '%s\n' "$bundle_signature" | grep -q 'flags=.*runtime' || \
  fail "bundle signature does not enable the hardened runtime"
printf '%s\n' "$bundle_signature" | grep -q '^Timestamp=' || \
  fail "bundle signature has no trusted timestamp"
/usr/bin/codesign --verify --deep --strict --verbose=2 "$bundle" || \
  fail "bundle signature verification failed"

xcrun stapler validate "$bundle" || fail "bundle has no valid stapled notarization ticket"
/usr/sbin/spctl --assess --type execute --verbose=4 "$bundle" || \
  fail "Gatekeeper rejected the bundle"

/usr/bin/unzip -tq "$archive" >/dev/null || fail "ZIP integrity check failed"
actual_contents=$(mktemp)
expected_contents=$(mktemp)
extracted=$(mktemp -d)
trap 'rm -f "$actual_contents" "$expected_contents"; rm -rf "$extracted"' EXIT HUP INT TERM
/usr/bin/unzip -Z1 "$archive" | LC_ALL=C sort >"$actual_contents"
cat >"$expected_contents" <<'EOF'
Spaceman.app/
Spaceman.app/Contents/
Spaceman.app/Contents/CodeResources
Spaceman.app/Contents/Info.plist
Spaceman.app/Contents/MacOS/
Spaceman.app/Contents/MacOS/spaceman
Spaceman.app/Contents/MacOS/spaceman-agent
Spaceman.app/Contents/_CodeSignature/
Spaceman.app/Contents/_CodeSignature/CodeResources
EOF
LC_ALL=C sort -o "$expected_contents" "$expected_contents"
/usr/bin/diff -u "$expected_contents" "$actual_contents" || \
  fail "ZIP contains missing or unexpected files"
/usr/bin/unzip -qq "$archive" -d "$extracted"
for relative_path in \
  Contents/CodeResources \
  Contents/Info.plist \
  Contents/MacOS/spaceman \
  Contents/MacOS/spaceman-agent \
  Contents/_CodeSignature/CodeResources
do
  /usr/bin/cmp "$bundle/$relative_path" "$extracted/Spaceman.app/$relative_path" || \
    fail "packed $relative_path differs from the validated bundle"
done
/usr/bin/codesign --verify --deep --strict --verbose=2 "$extracted/Spaceman.app" || \
  fail "packed bundle signature verification failed"
xcrun stapler validate "$extracted/Spaceman.app" || \
  fail "packed bundle has no valid stapled notarization ticket"

printf 'Release valid: version %s, macOS %s, %s, Developer ID signed, notarized, and Gatekeeper accepted\n' \
  "$cargo_version" "$deployment_target" "$expected_arch"
