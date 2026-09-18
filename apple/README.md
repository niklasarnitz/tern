# Tern macOS client

This package contains the first native macOS SwiftUI shell. It reads accounts,
mailboxes, and bounded message pages through the generated UniFFI application
API; it does not speak IMAP from Swift.

The Rust build should place the generated UniFFI files in `Generated/`:

- `mail_core.swift`, `mail_coreFFI.h`, and `mail_coreFFI.modulemap`
- `mail_model.swift`, `mail_modelFFI.h`, and `mail_modelFFI.modulemap`

Build the Rust library first, then run the app with a populated local database:

```sh
cargo build --manifest-path ../core/Cargo.toml -p mail-core
TERN_DATABASE=/path/to/mail.sqlite swift run
```

Without `TERN_DATABASE`, the app uses
`~/Library/Application Support/Tern/mail.sqlite`. The message list requests a
maximum of 100 summaries for the selected mailbox, so opening the app never
loads an entire mailbox into Swift memory.
