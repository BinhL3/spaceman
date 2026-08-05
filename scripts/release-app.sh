#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
identity=${SPACEMAN_CODESIGN_IDENTITY:-}
notary_profile=${SPACEMAN_NOTARY_KEYCHAIN_PROFILE:-}

[ -n "$identity" ] || {
  echo "SPACEMAN_CODESIGN_IDENTITY must name a Developer ID Application identity." >&2
  exit 1
}
[ -n "$notary_profile" ] || {
  echo "SPACEMAN_NOTARY_KEYCHAIN_PROFILE must name a notarytool keychain profile." >&2
  exit 1
}

case "$identity" in
  "Developer ID Application:"*) ;;
  *)
    echo "SPACEMAN_CODESIGN_IDENTITY must be a Developer ID Application identity, not '$identity'." >&2
    exit 1
    ;;
esac

[ -z "$(git -C "$root" status --porcelain)" ] || {
  echo "Release builds require a clean, reviewed Git worktree." >&2
  exit 1
}

xcrun notarytool history --keychain-profile "$notary_profile" >/dev/null || {
  echo "The notarytool keychain profile '$notary_profile' is missing or unusable." >&2
  exit 1
}

version=$(/usr/bin/plutil -extract CFBundleShortVersionString raw "$root/app/Info.plist")
arch=${SPACEMAN_RELEASE_ARCH:-$(uname -m)}
case "$arch" in
  arm64|x86_64) ;;
  *)
    echo "SPACEMAN_RELEASE_ARCH must be arm64 or x86_64, not '$arch'." >&2
    exit 1
    ;;
esac
dist="$root/dist"
artifact="$dist/Spaceman-v$version-macos-$arch.zip"
checksum="$artifact.sha256"
submission="$dist/.Spaceman-v$version-macos-$arch-notarization.zip"

mkdir -p "$dist"
rm -f "$artifact" "$checksum" "$submission"
trap 'rm -f "$submission"' EXIT HUP INT TERM

bundle=$(SPACEMAN_CODESIGN_IDENTITY="$identity" "$root/scripts/build-app.sh")
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
