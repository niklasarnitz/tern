# mail-sync

## Scope

`mail-sync` orchestrates credential lookup, bounded IMAP header/body fetching,
and local snapshot persistence. The development CLI currently calls this seam;
Swift only queries `mail-core`. Future Swift sync APIs should route through this crate,
which owns IMAP and SQLite sequencing. Inherit shared guidance from [`../../../AGENTS.md`](../../../AGENTS.md).

## Invariants

- `CredentialProvider` resolves an opaque `credential_ref`; the secret is used only for the network call and never persisted, logged, or included in errors.
- Complete the network fetch before `mail-db::Database::apply_snapshot` opens
  its write transaction. A timeout, TLS failure, authentication failure, or
  malformed response must leave the previous offline cache readable.
- Fetch headers first and fetch complete bodies only for bounded selected
  messages. Pass MIME bytes through `mail-mime`; preserve normalized content,
  threading metadata, and provider identity without putting secrets in the
  store.
- Mark/read, star, and move actions are offline per-membership operations.
  Queue them only after validating mailbox membership, replay with bounded
  retries, and let UIDVALIDITY changes mark stale operations rather than
  silently applying them to a new remote identity. Keep transaction ordering
  explicit: finish network work before opening the store write transaction.

## Coordination

Changes to the sync contract require coordination with `mail-imap`, `mail-db`, `mail-model`, and `mail-core`'s CLI/bridge callers. Any new persistence step
must document its network ordering and preserve offline failure behavior.

## Validation

From the repository root, run:

```sh
devbox run -- cargo test --manifest-path core/Cargo.toml -p mail-sync --locked
```

For contract or bridge-visible changes, run affected downstream tests and the
broader Rust/Swift checks required by the root guidance.

The current crate has no unit tests beyond compile/doc-test coverage; behavior
changes require affected producer/consumer or integration tests; zero tests are
not proof.
