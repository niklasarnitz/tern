# mail-mime

## Scope

`mail-mime` normalizes fetched RFC 5322 messages into `mail-model::RemoteHeader`
and bounded `MessageContent` records. The database and UI layers should not
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
- Normalize plain text, sanitized HTML, inline CID resources, binary/text
  attachments, and forwarded `message/rfc822` parts through `mail-parser`.
  Reject malformed transfer decoding and enforce the total decoded byte budget.
  Sanitized HTML removes active content and remote/relative image sources while
  preserving safe hyperlinks. Any web view remains responsible for locking
  navigation and network access.
- `conversation_plain_text` may remove only contiguous quote blocks whose
  unquoted lines exactly match a prior stored body. Preserve originals and use
  the result as a reversible display projection; leave HTML unchanged.

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
