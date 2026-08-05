# Security Policy

## Project Status

Spaceman is currently private and experimental. It has no supported public release or security
maintenance window. If the repository becomes public, this policy must be reviewed before release.

## Reporting A Vulnerability

Do not file a public issue for a suspected vulnerability or include private window/state data in a
report. Contact the repository owner privately; if GitHub private vulnerability reporting is enabled,
use the repository's **Security > Report a vulnerability** flow.

Include the affected revision, macOS version and architecture, impact, minimal reproduction, and any
suggested mitigation. Redact window titles, bundle IDs where sensitive, local paths beyond those
needed to explain the issue, and the contents of `parked-windows.json` or `workspace-state.json`.

The maintainer will acknowledge the report, assess scope, and coordinate disclosure when practical.
No response-time guarantee applies while the project remains private and experimental.

## Scope Notes

Spaceman has broad Accessibility permission and uses private `_AXUIElementGetWindow`. The local
control socket and lock are mode `0600`; there is currently no network, telemetry, or update-check
surface. See [Security and privacy](docs/security-and-privacy.md) for the current data model.
