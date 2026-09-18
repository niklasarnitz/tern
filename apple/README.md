# Tern macOS client

This package contains the first native macOS SwiftUI shell. It reads accounts,
mailboxes, and bounded message pages through the generated UniFFI application
API; it does not speak IMAP from Swift.

The Rust build should place the generated UniFFI files in `Generated/`:

- `mail_core.swift`, `mail_coreFFI.h`, and `mail_coreFFI.modulemap`
- `mail_model.swift`, `mail_modelFFI.h`, and `mail_modelFFI.modulemap`

Build the Rust library first, then run the app with a populated local database:

```sh
cd ..
TERN_DATABASE=/absolute/path/to/mail.sqlite devbox run macos

# Build, lint, analyze, and test the actual Rust–Swift bridge:
devbox run check-swift
```

Without `TERN_DATABASE`, the app uses
`~/Library/Application Support/Tern/mail.sqlite`. The message list requests a
maximum of 101 summaries (100 displayed plus one pagination probe), so opening the app never
loads an entire mailbox into Swift memory.
