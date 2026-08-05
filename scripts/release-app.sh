#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
notary_profile=${SPACEMAN_NOTARY_KEYCHAIN_PROFILE:-spaceman-notary}
host_arch=$(uname -m)
arch=${SPACEMAN_RELEASE_ARCH:-$host_arch}

case "$arch" in
  arm64|x86_64) ;;
  *)
    echo "SPACEMAN_RELEASE_ARCH must be arm64 or x86_64, not '$arch'." >&2
    exit 1
    ;;
esac
[ "$arch" = "$host_arch" ] || {
  echo "Cross-architecture release builds are not supported: requested '$arch' on '$host_arch'." >&2
  exit 1
}

[ -z "$(git -C "$root" status --porcelain)" ] || {
  echo "Release builds require a clean, reviewed Git worktree." >&2
  exit 1
}

xcrun notarytool history --keychain-profile "$notary_profile" >/dev/null || {
  echo "The notarytool keychain profile '$notary_profile' is missing or unusable." >&2
  exit 1
}

version=$(/usr/bin/plutil -extract CFBundleShortVersionString raw "$root/app/Info.plist")
dist="$root/dist"
artifact="$dist/Spaceman-v$version-macos-$arch.zip"
checksum="$artifact.sha256"
submission="$dist/.Spaceman-v$version-macos-$arch-notarization.zip"

mkdir -p "$dist"
rm -f "$artifact" "$checksum" "$submission"
trap 'rm -f "$submission"' EXIT HUP INT TERM

bundle=$("$root/scripts/build-app.sh")
(cd "$(dirname "$bundle")" && /usr/bin/zip -qry --symlinks "$submission" "$(basename "$bundle")")
notary_result=$(xcrun notarytool submit "$submission" \
  --keychain-profile "$notary_profile" --wait --output-format plist)
notary_status=$(printf '%s' "$notary_result" | /usr/bin/plutil -extract status raw -)
[ "$notary_status" = "Accepted" ] || {
  echo "Apple notarization finished with status '$notary_status', not Accepted." >&2
  exit 1
}
echo "Apple notarization status: $notary_status"
xcrun stapler staple "$bundle"

(cd "$(dirname "$bundle")" && /usr/bin/zip -qry --symlinks "$artifact" "$(basename "$bundle")")
SPACEMAN_RELEASE_ARCH="$arch" "$root/scripts/validate-release.sh" "$bundle" "$artifact"
(cd "$dist" && /usr/bin/shasum -a 256 "$(basename "$artifact")" >"$(basename "$checksum")")
(cd "$dist" && /usr/bin/shasum -a 256 -c "$(basename "$checksum")")

rm -f "$submission"
trap - EXIT HUP INT TERM
printf '%s\n' "$artifact"
printf '%s\n' "$checksum"
