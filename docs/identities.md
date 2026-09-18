# Multiple sender identities and aliases

Tern should model a mail account, a sender identity, and an SMTP transport as
separate concepts. One account owns the mailbox cache. It can have multiple
identities, and multiple identities can use the same authenticated SMTP
transport.

This distinction is required for iCloud+, where one iCloud mailbox and one set
of credentials can receive mail for several `@icloud.com` aliases and custom
domain addresses. It is also useful for generic IMAP accounts whose aliases use
different display names, reply addresses, signatures, or outgoing servers.

## Product behavior

Each sender identity has:

- a stable local ID and owning account ID;
- a From address and display name;
- an optional Reply-To address;
- a plain-text signature and, when rich composition exists, a sanitized HTML
  signature;
- an enabled/sendable flag and sort order;
- an SMTP transport reference; and
- a default flag, with exactly one default enabled identity per account.

The From menu in compose shows enabled identities for the selected account. A
new message starts with that account's default identity. Changing identity
updates the display name, Reply-To, and signature together, but never silently
replaces user-edited signature text. A draft persists the selected identity ID,
not a copied address, and warns instead of silently switching if that identity
is later disabled.

Aliases that can send are identities; they do not need a second account or a
second copy of the mailbox. Receive-only addresses can be retained for matching
and diagnostics, but must not appear in the From menu. Reply-To controls where
recipients answer and must not influence which identity Tern selects.

## Automatic identity selection for replies

Selection stays within the account that owns the original message. It uses
parsed mailbox addresses and case-insensitive normalized address comparison,
not substring matching.

1. Keep an identity already selected on a draft.
2. When replying to a message sent by the user, match the original `From`
   address. This preserves the identity used for that conversation.
3. Otherwise, choose the first enabled exact identity match in the original
   `To`, then `Cc` recipients.
4. If no visible recipient matches, try delivery addresses from the innermost
   `Delivered-To`, then `Envelope-To`, then `X-Original-To` header. These cover
   Bcc delivery and some forwarding paths.
5. Fall back to the account's default identity.

The compose UI always exposes the result and allows an override. Tern should
not infer a sendable identity from a domain-only match. In particular, a
catch-all message addressed to `anything@example.com` does not prove that the
SMTP provider permits that address in `From`.

This is intentionally more conservative than Thunderbird's configurable
catch-all identity support. Thunderbird also checks exact `To`/`Cc` matches,
then delivery headers, but can use a wildcard catch-all match as the literal
From address. That behavior is useful for providers that authorize arbitrary
domain senders; it is unsafe as a default for iCloud+.

The selection inputs must be persisted with the message before replies are
implemented. Tern already parses `To` and `Cc` into `RemoteHeader`, but the
current database drops them. Sync should additionally request and retain the
delivery headers without exposing raw message headers to Swift.

## SMTP ownership

An SMTP transport belongs to an account and contains host, port, TLS mode,
authentication username, and an opaque credential reference. An identity
references a transport; it does not duplicate secrets. Most accounts, including
iCloud, will have one transport shared by every identity. Keeping the reference
explicit still supports providers where one alias requires a different
transport.

Before submission, MIME construction uses the selected identity for `From`,
optional `Reply-To`, and the appropriate envelope sender policy. The SMTP
adapter must reject a disabled or missing identity before network I/O and must
surface provider rejection of an unauthorized From address. A successful SMTP
login does not establish that every configured From address is authorized.

## iCloud+ custom-domain findings

As of September 2026, Apple's published behavior is:

- [iCloud+ supports up to five custom domains and up to three personalized
  addresses per domain](https://support.apple.com/en-us/102540). Each invited
  person can have up to three active addresses per domain, and a domain can be
  shared with up to five other people.
- A primary iCloud Mail address is required. Custom addresses share the iCloud
  mailbox; they are identities under one Tern account, not separate accounts.
- iCloud Mail also supports [up to three `@icloud.com`
  aliases](https://support.apple.com/guide/icloud/add-and-manage-email-aliases-mm6b1a490a/icloud).
  Apple lets users send and receive through enabled aliases and custom-domain
  addresses, select enabled From addresses, and choose a default send address
  in iCloud Mail.
- [“Allow All” is a receive-only catch-all
  feature](https://support.apple.com/guide/icloud/allow-all-incoming-emails-mm9e3ee0680f/icloud).
  Unknown recipients arrive in the domain owner's inbox, but Apple does not say
  that those arbitrary recipients are authorized outbound From addresses.
  Tern must therefore require an explicitly configured active custom-domain
  address before offering it as a sender.
- Apple's [third-party client settings](https://support.apple.com/en-us/102525)
  specify `imap.mail.me.com:993` and `smtp.mail.me.com:587`, authenticated with
  the iCloud Mail address and an app-specific password. SMTP requires TLS and
  authentication. Tern should use explicit STARTTLS negotiation on port 587
  with no cleartext fallback, consistent with the transport policy in
  `docs/architecture.md`.
- Apple documents third-party SMTP and custom-domain sending separately, but
  does not explicitly document which login address third-party clients must use
  when sending from a custom address, nor an API for discovering all aliases.
  Tern should authenticate with the user's primary iCloud Mail login, collect
  identities during onboarding, and validate each custom From address with a
  live SMTP integration test. It should not scrape iCloud.com.
- iCloud.com's [signature is one plain-text setting applied to new
  mail](https://support.apple.com/guide/icloud/create-an-email-signature-mm6b1a3290/icloud),
  not a documented per-address synchronized value. Per-identity signatures in
  Tern are local Tern settings and will not synchronize with Apple's signature.
- Hide My Email relay addresses are not ordinary user-selectable SMTP
  identities. Supporting replies through Apple's relay is a separate feature
  and should not be inferred from alias support.

Because IMAP and SMTP do not provide a standard identity-discovery API, the
first provider-compatible version should use manual identity entry. Provider
onboarding may prefill known account addresses, but the user remains able to
add, disable, reorder, and test identities.

## Persistence and migration

Add `sender_identities` and `smtp_transports` tables rather than expanding the
single `accounts.email` field into provider-specific columns. Enforce a unique
normalized From address per account and one default identity per account in a
transaction. Deleting an account cascades to both tables; an identity referenced
by a draft is disabled rather than deleted until draft references are resolved.

The migration creates one default identity for every existing account from its
current `email` and `display_name`. Existing account credentials remain opaque.
`Account.email` can remain as a compatibility/display field until all callers
read the default identity, then be removed in a later contract migration.

The application API should expose bounded account identity lists and explicit
identity mutations. Network validation is an explicit operation, never a side
effect of an offline list call. Swift owns forms and selection state; Rust owns
normalization, invariants, persistence, reply selection, MIME fields, and SMTP
submission.

## Delivery order

1. Persist identities, transports, recipient/delivery-address metadata, and
   offline identity APIs. Add pure reply-selection tests covering To, Cc, Bcc,
   sent mail, disabled identities, cross-account aliases, and fallback.
2. Add account settings and onboarding UI for default identity, From name,
   Reply-To, signatures, and manual iCloud custom-domain addresses.
3. Build drafts and compose around a persisted identity ID, including safe
   signature replacement and visible manual selection.
4. Add `mail-smtp`, MIME construction, TLS/authentication, and provider-specific
   live tests. Verify primary, `@icloud.com` alias, each custom domain, rejected
   unconfigured catch-all sender, and Sent-folder behavior against an authorized
   iCloud test account.
5. Enable automatic reply selection only after recipient and delivery metadata
   are available for both newly synchronized and migrated messages.

This ordering avoids presenting identities that Tern cannot yet use, while
keeping the provider-specific uncertainty at the network boundary where it can
be tested.
