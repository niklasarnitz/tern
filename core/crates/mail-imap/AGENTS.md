# mail-imap

## Scope

`mail-imap` owns the first slice's read-only IMAP boundary: verified implicit
TLS, authenticated Inbox header snapshots, and selectable remote mailbox names.
It converts protocol data into `mail-model` snapshots through `mail-mime`.

## Invariants

- Use the platform certificate roots and implicit TLS. The current slice has no
  STARTTLS downgrade path, certificate override, or Gmail password login;
  Gmail requires the future OAuth path.
- `EXAMINE` and `BODY.PEEK` keep header sync read-only. Keep the bounded recent
  fetch, UIDVALIDITY/UIDNEXT metadata, deterministic UID ordering, and timeout
  behavior intact.
- Streaming loops may ignore unrelated typed responses, but must drain until
  the matching request tag and inspect its typed status. A matching tagged
  `NO`/non-OK is a protocol error; never discard completion status or accept a
  partial snapshot.
- Preserve sanitized `ImapError` values. Credentials, server response text,
  and account-sensitive diagnostics must not appear in errors or tracing.

## Coordination

Protocol or snapshot changes require coordination with `mail-mime`,
`mail-model`, `mail-sync`, and `mail-db` identity rules. STARTTLS, OAuth,
incremental sync, IDLE, mutations, bodies, and attachments are future slices;
do not introduce placeholder SMTP/events/OAuth implementations here.

## Validation

From the repository root, run:

```sh
devbox run -- cargo test --manifest-path core/Cargo.toml -p mail-imap --locked
```

For model/API or bridge-visible changes, also run the affected downstream tests
and the broader Rust/Swift checks required by the root guidance.
