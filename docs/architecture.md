# Tern architecture and delivery plan

Tern is a native Apple mail client with a portable Rust core and SQLite as its canonical local state. Swift calls an application API through UniFFI; Swift never owns IMAP, MIME, synchronization, or database SQL. A future Linux frontend will reuse that boundary.

## First delivery: cached Inbox headers

The first vertical slice imports up to 100 recent Inbox headers over verified implicit TLS, persists them transactionally, and displays the cache in a native macOS list after reopening without network. The CLI handles account configuration and credential prompting during this development slice. The UI is a cache viewer, not yet a complete mail client.

Crate dependencies flow from `mail-core` (application facade and CLI) through `mail-sync` (orchestration) to `mail-imap` (protocol) and `mail-db` (persistence). `mail-mime` normalizes headers through an existing MIME parser. `mail-model` carries the shared records. Add a real `mail-smtp` crate when SMTP is implemented, rather than an empty placeholder.

`mail-autoconfig` owns account discovery. It checks explicit overrides, known provider presets, HTTPS domain autoconfig, RFC 6186 DNS SRV records, and conservative hostname guesses in that order. Discovery probes verified implicit TLS and the IMAP greeting without authenticating. Unsupported STARTTLS discoveries are diagnostic only until the IMAP transport supports negotiated TLS without downgrade.

Database decisions:

- Account configuration stores credential references, never passwords or OAuth tokens.
- Remote UID identity is scoped by account, mailbox, and UIDVALIDITY. RFC Message-ID is metadata, not a unique key.
- A message and its mailbox membership are distinct. Read/star flags belong to membership for generic IMAP correctness. Gmail identity will require provider IDs before safely deduplicating across labels.
- A UIDVALIDITY reset invalidates the affected mailbox's UID mapping in the same transaction as the new snapshot. Other accounts/mailboxes survive.
- A bounded recent-header snapshot does not prove absence of older messages. Missing rows are therefore not deleted on repeat snapshot import. Deletion reconciliation belongs to incremental sync.
- WAL and foreign keys are enabled. Indexed, capped pages cross FFI; whole mailboxes do not.
- Networking completes before opening the write transaction. Failed connections preserve offline data.

Credentials are requested through a Rust `CredentialProvider`. The prototype CLI prompts without echo. Apple onboarding must implement Keychain before saving any credentials. Gmail must use OAuth, never password authentication. Implicit TLS is the only initial transport; STARTTLS needs explicit negotiation with no downgrade before enabling it.

## Following slices

1. Incremental sync: UID high-water marks, changed flags and deletion reconciliation. The network boundary already reconnects transient failures with bounded backoff, uses bounded IDLE with a polling fallback, and opens a fresh DNS/socket/TLS path on every attempt. CONDSTORE/QRESYNC/MOVE/UIDPLUS/SPECIAL-USE have conventional-IMAP fallbacks. UIDNEXT is not a message count. Empty UID ranges must not fetch older messages accidentally.
2. Message bodies: lazy fetch, MIME normalization, plain text and sanitized HTML in WKWebView. Block remote resources by default; restrict navigation, scripts, and local-file access. Inline content and attachment storage need explicit ownership and cleanup.
3. Offline actions: the first move slice now hides local membership and enqueues the operation atomically, offers an eight-second undo window, retains UIDVALIDITY and account scope, and replays expired operations during authenticated sync. Archive, delete, and spam use discovered destination mailboxes and safe atomic `UID MOVE`; unsupported servers fail without a broad `EXPUNGE` fallback. Flag changes, automatic reconnect/retry scheduling, conflict reconciliation, and richer destination projections remain future work.
4. SMTP and [sender identities](identities.md): local drafts/outbox, multiple From addresses, Reply-To, per-identity signatures, automatic reply identity selection, MIME construction, TLS, reply/reply-all/forward, attachments, and provider-specific Sent handling. Ambiguous delivery cannot be blindly retried.
5. Apple apps: shared application models, macOS sidebar/toolbar/menu/shortcuts and platform selection; iPhone navigation and iPad split view. Move blocking FFI reads off the main actor. iOS foreground/manual sync and opportunistic background refresh must tolerate suspension.
6. Gmail OAuth and iCloud onboarding: browser authorization, Keychain refresh tokens, provider discovery, diagnostics, Gmail label identity. Gmail/iCloud integration tests require authorized test accounts.
7. Hardening: 10 / 10,000 / 100,000+ messages; slow/offline/changing networks; disconnects; expired tokens; invalid certificates; UIDVALIDITY changes; duplicate IDs; malformed MIME; large attachments; multiple accounts. Measure launch, scrolling, RAM, CPU, DB size, sync time and battery.

Search can follow MVP using FTS5; threading initially remains a flat list. Deferred: Exchange/Graph/JMAP, calendars/contacts, encryption, rules, smart mailboxes, unified inbox, snooze/scheduling, AI, plugins, Linux UI and advanced previews.

An optional [push-trigger service](push-service.md) is implemented as an independently deployed wake relay. It stores only an opaque subscription-to-APNs-token route and never receives provider credentials, mailbox state, message metadata or message content. Provider adapters reduce native webhook events to an empty trigger; the device still synchronizes directly with the provider. Device registration and background handling remain gated on the future signed iOS target and incremental account-sync API.

## Public MVP acceptance

Both Apple apps install; users add generic IMAP, iCloud and Gmail accounts; folders and cached messages reopen instantly offline; text/HTML mail is readable; read/star/archive/delete/move reconcile after reconnect; compose/reply/forward/send work; mailbox updates are incremental. This first slice does not satisfy public MVP acceptance.
