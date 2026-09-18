# mail-mime

## Scope

`mail-mime` normalizes fetched RFC 5322 headers into `mail-model::RemoteHeader`.
The current boundary is header metadata only; database and UI layers should not
depend on `mail-parser` types. Inherit shared guidance from [`../../../AGENTS.md`](../../../AGENTS.md).

## Invariants

- Parsing is best-effort for malformed or incomplete headers: retain the IMAP
  UID and server flags, and use stable empty values for fields that cannot be
  decoded.
- Preserve decoded Message-ID, subject, first sender address, and header date
  semantics. IMAP `INTERNALDATE` is the synchronization date when available;
  callers may replace the parsed Date accordingly.
- Keep this crate free of mailbox identity and persistence policy. Message-ID
  remains metadata; `mail-db` owns local identity and membership state.
- Body parts, HTML sanitization, attachments, MIME construction, and SMTP are
  future slices. Do not widen this parser implicitly while changing headers.

## Coordination

Changes to `RemoteHeader` or normalization semantics require coordination with
`mail-model`, `mail-imap`, `mail-db`, and any `mail-core`/Apple consumers that
display the fields. Add fixtures for malformed and encoded headers when parser
behavior changes.

## Validation

From the repository root, run:

```sh
devbox run -- cargo test --manifest-path core/Cargo.toml -p mail-mime --locked
```

For shared record or bridge-visible changes, run the affected downstream tests
and the broader Rust/Swift checks required by the root guidance.
