# Optional push-trigger service

Tern's push service is a wake relay, not a mail proxy. It lets a provider
adapter nudge an Apple device when provider-native device push is unavailable.
The notification carries no mailbox or message state. On receipt, the device
maps an opaque subscription ID to an account held in its local database and
synchronizes directly with the mail provider using credentials held by the
device.

```text
┌──────────────────┐  provider event  ┌──────────────────┐
│ Provider webhook │─────────────────▶│ Provider adapter │
└──────────────────┘                  └────────┬─────────┘
                                               │ POST {} for opaque ID
                                               ▼
                                      ┌──────────────────┐
                                      │ Tern wake relay  │
                                      │ ID → APNs token  │
                                      └────────┬─────────┘
                                               │ silent notification
                                               ▼
┌───────────────┐   direct sync       ┌──────────────────┐
│ Mail provider│◀─────────────────────│ Tern on device   │
└───────────────┘                     └──────────────────┘
```

The provider adapter is deliberately separate from the relay. A provider's
webhook may contain an address or change cursor needed to route an event, but
the adapter reduces that event to an opaque ID before calling the relay. A
deployment can isolate or replace adapters without expanding the relay's data
model.

## Privacy and trust boundaries

The relay persists exactly two values per subscription:

- a device-generated 256-bit random value encoded as 43 unpadded base64url
  characters; and
- the opaque APNs device token encoded as hexadecimal.

It does not accept an email address, provider account ID, OAuth token, IMAP
credential, mailbox name, UID, sender, subject, message identifier, change
cursor, or message body. The trigger request is the closed JSON object `{}`;
unknown fields are rejected. The APNs payload is:

```json
{
  "aps": { "content-available": 1 },
  "tern": { "subscription_id": "<opaque 256-bit value>" }
}
```

APNs device tokens and subscription IDs are still sensitive routing metadata.
The registry and logs must remain private, and backups need the same controls
as other production secrets. The executable emits no request data.

There is no honest content-blind, event-driven relay for a generic IMAP server
that exposes neither a provider webhook nor device push. Maintaining IMAP IDLE
on the relay would require handing it an account credential that can generally
read mail. Tern therefore does not do that. Such accounts use foreground/manual
sync and OS-scheduled background refresh; a deployment may send blind periodic
wakes, but those are not evidence that mail changed.

## HTTP contract

All request bodies are limited to 1 KiB. IDs and tokens belong in request bodies
or paths only and must not be logged by an ingress proxy.

| Method and path | Authorization | Body | Result |
| --- | --- | --- | --- |
| `GET /healthz` | none | none | `204` when the process is serving |
| `PUT /v1/subscriptions/{id}` | registration bearer | `{"device_token":"<hex>"}` | create or rotate a route |
| `DELETE /v1/subscriptions/{id}` | registration bearer | none | remove a route |
| `POST /v1/subscriptions/{id}/trigger` | trigger bearer | `{}` | submit one APNs background wake |

Registration and trigger credentials are intentionally distinct. The static
environment values are deployment credentials, not secrets to embed in the
app. A public deployment must put the registration route behind a trusted
issuer that validates the signed-in user and Apple App Attest, then authorizes
only that user's short-lived registration. Provider adapters call the trigger
route over a private network or authenticated service mesh. Apply TLS, rate
limits, replay controls, and ingress request-size limits there as defense in
depth.

## Running the relay

The `mail-push-service` binary requires:

| Variable | Meaning |
| --- | --- |
| `TERN_PUSH_BIND` | private listen address, for example `127.0.0.1:8080` |
| `TERN_PUSH_DATABASE` | path to the private SQLite route registry |
| `TERN_PUSH_REGISTRATION_TOKEN` | edge-to-relay registration credential |
| `TERN_PUSH_TRIGGER_TOKEN` | adapter-to-relay trigger credential |
| `TERN_APNS_KEY_PATH` | path to Apple's `.p8` signing key |
| `TERN_APNS_KEY_ID` / `TERN_APNS_TEAM_ID` | Apple token-auth identifiers |
| `TERN_APNS_TOPIC` | signed app bundle identifier |
| `TERN_APNS_ENVIRONMENT` | `sandbox` or `production` |

Run it behind a TLS-terminating edge; the binary intentionally serves plain
HTTP only on its private bind address. APNs uses token authentication over
verified HTTP/2 TLS. Never commit any listed credential or the registry.

## Device behavior and delivery semantics

Push remains optional. The device must treat each wake as a lossy hint:

1. Validate the payload shape and look up the opaque ID locally.
2. Coalesce concurrent wakes for the same account.
3. Start the normal account synchronization path if background execution time
   is available; never fetch through the relay.
4. Report the OS background completion result and preserve cached mail on any
   network or credential failure.
5. Refresh normally on foreground entry even if no wake arrived.

APNs can delay, merge, throttle, or discard background notifications. Correctness
must never depend on one notification corresponding to one provider event.
Device registration and wake handling belong in the future signed iOS target;
the current macOS cache viewer has no background sync API to wire safely.
