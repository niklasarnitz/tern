# First-slice validation — 18 September 2026

Validated locally on Apple Silicon macOS with Xcode 26.3 / Swift 6.2.4 and Devbox-managed Rust 1.97.1.

`devbox run validate` passed on the merged implementation:

- Rustfmt; Clippy all + pedantic, all targets/features, warnings denied; rustdoc warnings denied.
- 19 Rust tests: 8 database, 8 IMAP, 2 MIME, 1 persisted application API integration.
- RustSec advisory audit with warnings denied; dependency license, registry/source and version-policy checks.
- SwiftLint strict: zero violations; SwiftFormat: no changes required.
- Swift 6 compilation and static linking against the actual generated UniFFI bridge, with compiler warnings as errors.
- 2 Swift integration tests: reopening a Rust-created SQLite cache through UniFFI, and native view-model paging through 100 + 5 headers without accumulating the whole mailbox.
- SwiftLint unused-declaration/import analysis from a clean compiler log: zero violations.

The TLS integration test runs a local scripted server and validates login, read-only EXAMINE, header FETCH and logout. A separate test rejects an untrusted certificate before credentials are sent. A failed tagged response after 100 headers is rejected. SQLite tests cover persistence, UIDVALIDITY changes, account isolation, duplicate Message-ID values, bounded pagination, oversized-snapshot rejection, transaction rollback and remote identity changes.

Reviewed branches merged into `master`:

- `feature/local-mail-store`: requested fixes for silent truncation, account identity reuse and orphan cleanup indexing before merge.
- `feature/imap-headers`: requested fixes for TLS provider selection, bounded fetching and explicit successful command completion before merge.
- `feature/macos-viewer`: requested native selection, bounded paging, stale-result protection and reload behavior before merge.

Local pre-commit and pre-push hooks are enabled through `core.hooksPath=.githooks`. CI definitions are present. There is no configured Git remote, so hosted CI and server-side required-status/branch-protection settings have not been exercised.

Still unverified: live generic IMAP/iCloud/Gmail accounts, populated-window visual acceptance, devices and large-mailbox performance. The development executable was launched, but the computer-use tool could not resolve the unbundled SwiftPM process for visual inspection. Gmail OAuth, message bodies, offline mutations, SMTP and the iOS app are future slices, not completed features.
