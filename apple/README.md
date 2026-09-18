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

The run script builds and registers a development `Tern.app` bundle. Its URL
type declaration lets macOS offer Tern as a handler for `mailto:` links. Opening
one creates a compose draft with its recipients, Cc/Bcc, subject, body, and any
local-file `attach` or `attachment` parameters. SMTP is not implemented yet, so
the draft can be reviewed but not sent.

Without `TERN_DATABASE`, the app uses
`~/Library/Application Support/Tern/mail.sqlite`. The message list requests a
maximum of 101 summaries (100 displayed plus one pagination probe), so opening the app never
loads an entire mailbox into Swift memory.

## Widget extension sources

`Sources/TernWidgets/TernWidgets.swift` contains macOS/iOS WidgetKit configurations for unread mail,
recent starred mail, and selected-mailbox counts. The app publishes a bounded, privacy-filtered
snapshot into the `group.com.niklasarnitz.tern` app group; no extension opens SQLite or links the
Rust core. Counts-only sharing is the default and no mailbox is selected by default.

The current SwiftPM executable cannot package an `.appex`. When Tern gains its installable Xcode
app targets, add `Sources/TernWidgets/TernWidgets.swift` and `Sources/Tern/WidgetShared.swift` to a Widget
Extension target on iOS and macOS, and give both the app and extension the app-group entitlement.
Quick compose is intentionally not advertised yet because Tern has no compose or SMTP operation.
