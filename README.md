# Tern

A native Apple mail client with a portable Rust core and an offline-first SQLite store.

**Current scope: early vertical slices, not the public MVP.** The Rust CLI imports the latest 100 Inbox headers and discovers selectable mailboxes; a macOS SwiftUI viewer reads and updates the persisted cache through UniFFI. No IMAP calls or SQL live in Swift. Account setup currently uses the CLI.

## Development environment

Install [Devbox](https://www.jetify.com/docs/devbox/installing_devbox/) and Xcode (for macOS). Rust, Cargo, rustfmt and Clippy are managed by this repository's `devbox.json` / `devbox.lock`. No global rustup installation is needed. SwiftLint, SwiftFormat, cargo-audit and cargo-deny are pinned as required quality tools. Cargo's cache lives in `.devbox/cargo`.

```sh
devbox install
devbox run setup-hooks
devbox run validate
```

## Import one Inbox

Discover settings from an address first. The report includes the selected candidate, alternatives, authentication type, discovery source, and sanitized TLS/IMAP connection diagnostics. Known Gmail, iCloud, Outlook, Yahoo, AOL, and Fastmail domains use presets; other domains use HTTPS autoconfig, RFC 6186 DNS SRV records, then `imap.<domain>` / `mail.<domain>` fallbacks. Discovery never authenticates.

```sh
devbox run -- cargo run --manifest-path core/Cargo.toml --bin tern -- \
  discover-account you@example.com

# Any discovered value can be overridden and rechecked:
devbox run -- cargo run --manifest-path core/Cargo.toml --bin tern -- \
  discover-account you@example.com \
  --imap-host mail.example.com --imap-port 993 --username login-name
```

Copy `docs/account.example.json` and apply the recommended configuration, or provide manual values when no candidate succeeds. Do not add passwords to that file. The initial transport is verified implicit TLS, normally port 993. iCloud requires its app-specific password. Discovery reports Gmail and Outlook OAuth requirements, but Gmail synchronization remains intentionally rejected until OAuth is implemented.

```sh
devbox run -- cargo run --manifest-path core/Cargo.toml --bin tern -- \
  --database "$PWD/tern.sqlite" add-account /absolute/path/to/account.json

devbox run -- cargo run --manifest-path core/Cargo.toml --bin tern -- \
  --database "$PWD/tern.sqlite" sync personal
```

The sync command prompts for the password without echo; it is not persisted. The import reads headers and flags without marking messages read. It also replays expired move operations before refreshing Inbox. Repeated imports update the recent window; this is not yet incremental sync or deletion reconciliation.

## Browse offline

After a successful import, disconnect from the network and run:

```sh
devbox run -- cargo run --manifest-path core/Cargo.toml --bin tern -- \
  --database "$PWD/tern.sqlite" mailboxes personal

# Use a mailbox id returned above:
devbox run -- cargo run --manifest-path core/Cargo.toml --bin tern -- \
  --database "$PWD/tern.sqlite" messages MAILBOX_ID --limit 100

TERN_DATABASE="$PWD/tern.sqlite" devbox run macos
```

The first macOS build needs network access to download build dependencies; once built, the application reads the cache entirely offline. Without `TERN_DATABASE`, the app uses its Application Support directory. Select an account and Inbox to browse headers. Archive, delete, spam, and arbitrary move actions are applied locally and offer Undo for eight seconds; after that window, the next authenticated CLI sync sends them with IMAP `UID MOVE`. Empty databases display an empty state, not sample mail.

## Layout and next steps

- `core/crates/mail-model`: portable records
- `mail-db`: SQLite migration, identities, transactions and capped pages
- `mail-autoconfig`: provider, HTTPS autoconfig, DNS SRV, fallback and connection diagnostics
- `mail-imap`, `mail-mime`: secure IMAP header fetch and MIME normalization
- `mail-sync`: credential abstraction and import orchestration
- `mail-core`: application API, UniFFI and development CLI
- `mail-push-service`: optional content-free APNs wake relay
- `apple`: native macOS SwiftUI cache viewer

See [architecture and milestones](docs/architecture.md), [push service design and deployment contract](docs/push-service.md), [dependency decisions](docs/dependencies.md), [Apple build notes](apple/README.md), and [required quality gates](docs/quality.md). See [local validation and remaining gates](docs/validation.md) for tested scope.

Still ahead: all-folder content sync, incremental updates and events, non-move offline mutations, message bodies/HTML, SMTP/compose and undo-send, Apple Keychain onboarding, Gmail OAuth, an installable iOS target, and provider/device/performance validation. The Swift package is a development macOS executable, not a signed app distribution. Credentials for live mail accounts are not included.

## Agent guidance

Start with [AGENTS.md](AGENTS.md) for architecture boundaries, subagent worktrees, review/merge policy and required checks. Each Rust crate, the Apple app and build scripts have scoped `AGENTS.md` instructions.
