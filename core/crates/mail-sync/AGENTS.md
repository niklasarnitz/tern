# mail-sync

## Scope

`mail-sync` orchestrates credential lookup, read-only IMAP fetching, and local
snapshot persistence. Frontends call this seam; they do not own IMAP or SQLite
transaction sequencing.

## Invariants

- `CredentialProvider` resolves an opaque `credential_ref`; the secret is used
  only for the network call and never persisted, logged, or included in errors.
- Complete the network fetch before `mail-db::Database::apply_snapshot` opens
  its write transaction. A timeout, TLS failure, authentication failure, or
  malformed response must leave the previous offline cache readable.
- The implemented operation is a bounded Inbox header snapshot. Preserve
  account and mailbox identity rules from `mail-db`, and propagate sanitized
  errors without inventing remote behavior.
- Incremental reconciliation, flags/actions, retries, IDLE, OAuth, and body
  sync are future slices. Keep orchestration changes explicit about their new
  transaction and conflict semantics.

## Coordination

Changes to the sync contract require coordination with `mail-imap`, `mail-db`,
`mail-model`, and `mail-core`'s CLI/bridge callers. Any new persistence step
must document whether it runs before or after network work and preserve the
offline failure behavior.

## Validation

From the repository root, run:

```sh
devbox run -- cargo test --manifest-path core/Cargo.toml -p mail-sync --locked
```

For contract or bridge-visible changes, run affected downstream tests and the
broader Rust/Swift checks required by the root guidance.
