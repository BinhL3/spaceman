# Releasing

Spaceman is not release-ready merely because `scripts/build-app.sh` produces a signed bundle. The
script builds locked release binaries, assembles `Spaceman.app`, applies hardened-runtime Developer ID
signing with a timestamp, and verifies the signature. It does not create, notarize, staple, publish,
or update an artifact.

## Current Build

```sh
SPACEMAN_CODESIGN_IDENTITY="Developer ID Application: Example (TEAMID)" \
  ./scripts/build-app.sh
codesign --verify --deep --strict target/Spaceman.app
```

Do not publish `target/Spaceman.app` as a release based only on those commands.

## Release Blockers

- Notarize the final distributable with Apple and staple the ticket where the artifact format allows.
- Produce a downloadable artifact with an integrity checksum and documented install path.
- Decide and validate supported architectures, including universal packaging if promised.
- Validate the declared minimum macOS version and a current macOS version.
- Test first install, Accessibility onboarding, launch-at-login registration and relaunch, shortcuts,
  displays, upgrade, crash recovery, pause/quit restoration, and uninstall on a clean machine.
- Add and verify a concise product demo.
- Recheck the private `_AXUIElementGetWindow` risk and state clearly that the Mac App Store is not
  supported.
- Review `SECURITY.md`, `CONTRIBUTING.md`, license, version, changelog/release notes, and repository
  visibility before a public release.

## Release Rules

1. Release from a clean, reviewed commit and retain exact source revision and toolchain metadata.
2. Run all verification in [Development](development.md), both isolated live harnesses, and
   `git diff --check`.
3. Build once for release; notarize and distribute that exact artifact rather than rebuilding it.
4. Verify signature, notarization/stapling, archive contents, executable architecture, plist minimum
   OS, and checksum after packaging.
5. Install the downloadable artifact on a clean supported Mac and execute the recovery/uninstall path.
6. Never ask users to disable SIP or delete parked-window metadata to complete an install or upgrade.
7. Keep source-build instructions separate from artifact installation so users are not told they need
   a Developer ID identity for a future release.
