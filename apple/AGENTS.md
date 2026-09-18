# Apple frontend

Inherit [root instructions](../AGENTS.md). This directory currently contains a macOS SwiftPM executable and tests, not an installable iOS target or signed distribution. Read [build notes](README.md) when running it.

## Boundaries and behavior

- `Sources/Tern/MailStore.swift` owns presentation state; its background actor wraps the Rust `MailClient`. Keep blocking database/FFI work off the main actor and observable UI state on the main actor.
- `Sources/Tern/TernApp.swift` owns native SwiftUI navigation, lists and detail presentation. Preserve macOS keyboard selection, toolbar/menu commands and system appearance. Implement future iOS navigation and lifecycle as platform-specific behavior.
- Read local data through the application API. Add mail behavior in Rust before exposing it to Swift; Swift does not issue IMAP commands or SQL.
- Paging replaces the displayed window. The current extra-row probe establishes whether another page exists; an exactly full page alone does not establish that. Reset paging and selection when changing mailboxes.
- Conversations are the presentation unit when the application API exposes them: keep thread summaries bounded, load message metadata first, and request body/attachment content only for the selected detail. Render normalized MIME HTML as untrusted content; preserve the reversible plain-text quote toggle and keep navigation/network policy locked down in any web view.
- Mailbox flags and offline actions are per-membership state. Optimistic mark/star/move projections must remain identifiable until replay, show pending/failed state, and use the store's bounded retry/replay contract rather than inventing local persistence.
- Account/mailbox changes can overlap suspended requests. Apply results, errors and loading-state changes only to the request that still owns the current selection. Reload must discover newly configured accounts and newly cached mailboxes.
- Keep empty, loading, error and retry states usable. Display fixture mail only through explicitly selected fixture databases.

## Bridge and tooling

`Generated/` is produced by `devbox run bindings` and is ignored by Git. Change Rust declarations or generation configuration instead of editing generated Swift/C files. `Package.swift` links the static Rust archive explicitly. Coordinate API/record changes with `mail-core` and `mail-model`, then regenerate and compile both sides.

Use `devbox run lint-swift` for focused style checks and `devbox run check-swift` for compilation, integration tests and analyzer checks. The latter creates an isolated Rust fixture and supplies `TERN_DATABASE` and `TERN_TEST_DATABASE`; running those tests without the harness omits their required database. Use the Xcode wrapper in `scripts/` when extending tooling, so Nix's SDK environment does not break SourceKit.

Changes are ready when native state behavior is verified, the real generated bridge compiles under Swift 6 with warnings as errors, and the Apple checks pass. A successful build or view-model test does not establish visual, accessibility or device acceptance; report those separately.
