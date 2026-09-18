# mail-model

## Scope

`mail-model` owns portable records shared by Rust and UniFFI; inherit shared guidance from [`../../../AGENTS.md`](../../../AGENTS.md). Exported records are
`Account`, `Mailbox`, `MessageSummary`, conversation records, and pending
operation records cross the Rust/Swift boundary; `RemoteHeader` and
`MailboxSnapshot` are internal synchronization inputs.

## Invariants

- Records contain configuration and cache data only. `credential_ref` is an
  opaque reference; passwords and OAuth tokens never enter a record.
- `Mailbox` carries optional UID metadata. A snapshot's `remote_name` and
  UIDVALIDITY pair with the `account_id` supplied to `mail-db`; `MessageSummary.id` is a local row identity.
- `MessageSummary.is_read` and `is_starred` describe mailbox membership state.
  `RemoteHeader` preserves the server UID and flags while MIME normalizes text.
- `MessageContent` is normalized MIME data with bounded plain text, sanitized
  HTML, and attachment bytes. `ThreadMessage.content` is optional so list
  paths can remain lazy. Membership flags and pending operation replay state
  belong to mailbox/action records, not message identity.

## Coordination

Changing an exported record or field is a contract change: coordinate with `mail-core`'s UniFFI API, Apple generated bindings and consumers, plus
`mail-db`, `mail-imap`, `mail-mime`, and `mail-sync` as applicable. Update
serialization and fixtures with the contract rather than adding parallel local
record types.

## Validation

From the repository root, run:

```sh
devbox run -- cargo test --manifest-path core/Cargo.toml -p mail-model --locked
```

For exported API changes, also run `devbox run check-swift` and the binding
generation/checks required by the root guidance.

The current crate has no unit tests beyond compile/doc-test coverage; behavior
changes require affected producer/consumer or integration tests; zero tests are
not proof.
