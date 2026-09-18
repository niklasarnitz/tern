# mail-push-service

## Scope

This crate is the optional, independently deployed wake relay. It is not linked
into the mail client and must not depend on mail records, IMAP, credentials, or
the local database. Inherit shared guidance from [`../../../AGENTS.md`](../../../AGENTS.md).

## Invariants

- The service accepts opaque subscription IDs and APNs device tokens only. Its
  trigger endpoint has an empty, closed schema. Do not add account addresses,
  provider credentials, mailbox state, message metadata, or message content.
- A notification is only a hint. The device maps the opaque ID to a local
  account and synchronizes directly with the provider; delivery can be delayed,
  duplicated, coalesced, or dropped.
- Registration and trigger ingress use separate credentials. Never put either
  credential, an APNs token, or a subscription ID in logs or errors.
- Keep the SQLite registry private and bounded operationally. Internet-facing
  deployments require TLS, request-size limits, rate limits, and a trusted
  issuer for short-lived registration grants at the edge.

## Validation

From the repository root, run:

```sh
devbox run -- cargo test --manifest-path core/Cargo.toml -p mail-push-service --locked
```
