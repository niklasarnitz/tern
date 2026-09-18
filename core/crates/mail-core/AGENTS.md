# mail-core

## Scope

`mail-core` is the stable application facade exposed to Apple through UniFFI.
The current `MailClient` opens the canonical local store and offers read-only
account, mailbox, bounded message-list, conversation, and pending-operation
queries. Message bodies and attachments are loaded through the selected detail
path, while list APIs remain metadata-only. The CLI is the development owner
of account configuration and sync. Inherit shared guidance from
[`../../../AGENTS.md`](../../../AGENTS.md).

## Invariants

- Keep the foreign API expressed in `mail-model` records and sanitized
  `MailError` values. Exported methods must remain safe to call after reopening
  an offline database and must not perform network work implicitly.
- Message and conversation pages remain bounded by the database contract. Keep
  body/attachment payloads lazy and avoid exposing whole mailboxes or
  SQLite/IMAP implementation types across UniFFI.
- The CLI may prompt for a password without echo and invoke `mail-sync`; JSON
  account configuration stores a credential reference only. Apple onboarding
  must later supply Keychain-backed credentials.
- The first slice is not a complete mail client. Compose, SMTP, actions,
  incremental sync, OAuth onboarding, and background refresh are future work.

## Coordination

Any exported method, record, error, or ownership change requires coordination
with `mail-model`, `mail-db`, `mail-sync`, generated UniFFI bindings, and Apple
callers. Regenerate bindings from the generator/configuration rather than
editing generated Swift by hand.

## Validation

From the repository root, run:

```sh
devbox run -- cargo test --manifest-path core/Cargo.toml -p mail-core --locked
```

For UniFFI or Apple-facing changes, also run binding generation, `devbox run check-swift`,
and the broader validation required by the root guidance.
