# Tern

A native Apple mail client with a portable Rust core and an offline-first SQLite store.

**Current scope: first vertical slice, not the public MVP.** The Rust CLI imports the latest 100 Inbox headers; a macOS SwiftUI viewer reads the persisted cache through UniFFI. No IMAP calls or SQL live in Swift. Account setup currently uses the CLI.

## Development environment

Install [Devbox](https://www.jetify.com/docs/devbox/installing_devbox/) and Xcode (for macOS). Rust, Cargo, rustfmt and Clippy are managed by this repository's `devbox.json` / `devbox.lock`. No global rustup installation is needed. Cargo's cache lives in `.devbox/cargo`.

```sh
devbox install
devbox run test
devbox run check
devbox run fmt
```

## Import one Inbox

Copy `docs/account.example.json` and replace the example configuration. Do not add passwords to that file. The initial transport is verified implicit TLS, normally port 993. iCloud requires its app-specific password. Gmail is intentionally rejected until OAuth is implemented.

```sh
devbox run -- cargo run --manifest-path core/Cargo.toml --bin tern -- \
  --database "$PWD/tern.sqlite" add-account /absolute/path/to/account.json

devbox run -- cargo run --manifest-path core/Cargo.toml --bin tern -- \
  --database "$PWD/tern.sqlite" sync personal
```

The sync command prompts for the password without echo; it is not persisted. The import reads headers and flags without marking messages read. Repeated imports update the recent window; this is not yet incremental sync or deletion reconciliation.

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

The first macOS build needs network access to download build dependencies; once built, the application reads the cache entirely offline. Without `TERN_DATABASE`, the app uses its Application Support directory. Select an account and Inbox to browse headers. Empty databases display an empty state, not sample mail.

## Layout and next steps

- `core/crates/mail-model`: portable records
- `mail-db`: SQLite migration, identities, transactions and capped pages
- `mail-imap`, `mail-mime`: secure IMAP header fetch and MIME normalization
- `mail-sync`: credential abstraction and import orchestration
- `mail-core`: application API, UniFFI and development CLI
- `apple`: native macOS SwiftUI cache viewer

See [architecture and milestones](docs/architecture.md), [dependency decisions](docs/dependencies.md), and [Apple build notes](apple/README.md).

Still ahead: all-folder sync, incremental updates and events, message bodies/HTML, offline mutations, SMTP/compose, Apple Keychain onboarding, Gmail OAuth, an installable iOS target, and provider/device/performance validation. The Swift package is a development macOS executable, not a signed app distribution. Credentials for live mail accounts are not included.
