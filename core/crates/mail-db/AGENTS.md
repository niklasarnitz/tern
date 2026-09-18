# mail-db

## Scope

`mail-db` is the canonical SQLite store for account configuration, mailboxes, message summaries, and mailbox membership. It maps `mail-model` records to
schema rows and exposes bounded offline reads plus transactional snapshots.
Inherit shared guidance from [`../../../AGENTS.md`](../../../AGENTS.md).

## Invariants

- Remote UID identity is scoped by account, mailbox, and UIDVALIDITY. RFC
  Message-ID is metadata and never a unique message identity.
- A message row and its mailbox membership are distinct. Read/star flags belong
  to `mailbox_messages`, so the same message can have different flags per
  mailbox.
- `apply_snapshot` is bounded, additive for unchanged UIDVALIDITY, and
  invalidates only the affected mailbox's memberships when UIDVALIDITY changes.
  Reject an oversized snapshot before any database mutation; do not delete absent older rows from a recent-header snapshot.
- Once cached, changing an account's host, port, or username requires a new
  account identity rather than reusing the existing cache.
- Foreign keys and WAL are enabled during open; schema changes require a
  reviewed migration and must preserve existing offline data.
- This crate persists credential references only. Networking belongs above the
  database; future sync must complete network work before opening its write
  transaction.

## Coordination

Schema, identity, or transaction changes require review with `mail-model`, `mail-sync`, and `mail-core`'s offline API. Changes to membership semantics
also need Apple/UI consumers and migration fixtures checked. Incremental deletion
reconciliation and offline actions are future slices, not implied by this importer.

## Validation

From the repository root, run:

```sh
devbox run -- cargo test --manifest-path core/Cargo.toml -p mail-db --locked
```
For schema/public API changes, also run affected `mail-core` tests and the broader Rust/Swift checks required by the root guidance.
