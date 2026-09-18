# mail-imap

## Scope

`mail-imap` owns the verified implicit-TLS IMAP boundary: authenticated mailbox
snapshots, bounded header/body fetches, and remote mailbox operations. It
converts protocol data into `mail-model` records through `mail-mime`.
Inherit shared repository guidance from [`../../../AGENTS.md`](../../../AGENTS.md);
dependency notes are in [`../../../docs/dependencies.md`](../../../docs/dependencies.md).

## Invariants

- Use the platform certificate roots and implicit TLS. The current slice has no
  STARTTLS downgrade path, certificate override, or Gmail password login;
  Gmail requires the future OAuth path.
- `EXAMINE` and `BODY.PEEK` keep reads read-only. `UIDSTORE` and `MOVE` execute
  remote mutations only through the sync operation path. Keep bounded recent header and
  body fetches, UIDVALIDITY/UIDNEXT metadata, deterministic UID ordering, and
  timeout behavior intact. Body fetches must honor the MIME byte budget before
  handing data to normalization.
- `async-imap` convenience streaming helpers can discard tagged failure status.
  Retain the `run_command`/`read_response` typed adapter: drain until the
  matching request tag, inspect its status, and reject tagged `NO`/non-OK.
- Preserve sanitized `ImapError` values. Credentials, server response text,
  and account-sensitive diagnostics must not appear in errors or tracing.

## Coordination

Protocol or snapshot changes require coordination with `mail-mime`,
`mail-model`, `mail-sync`, and `mail-db` identity rules. IDLE is limited to a
bounded change notification with polling fallback; reconnect policy belongs to
`mail-sync`. STARTTLS, OAuth, incremental reconciliation, and SMTP remain future
slices. Message bodies and attachments flow through the bounded MIME
normalization path; mutations remain owned by store/sync orchestration. Do not
introduce placeholder implementations here.

## Validation

From the repository root, run:

```sh
devbox run -- cargo test --manifest-path core/Cargo.toml -p mail-imap --locked
```
For model/API or bridge-visible changes, also run the affected downstream tests
and the broader Rust/Swift checks required by the root guidance.
