# Initial dependency decisions

Validated against upstream documentation and the repository build. `core/Cargo.lock` records exact Rust dependencies; `devbox.lock` records the Nix toolchain resolution.

| Concern | Initial choice | Reason / boundary |
| --- | --- | --- |
| Runtime | Tokio | Timeouts and asynchronous network I/O; core owns execution. |
| IMAP | async-imap | Existing protocol parser. The adapter uses typed responses and requires tagged OK completion; see the note below. Provider/extension behavior still needs live validation. |
| TLS | Rustls | Verified encrypted transport. No certificate-bypass mode. |
| MIME | mail-parser | Decode real mail headers using a maintained MIME implementation. Body normalization follows later. |
| SQLite | rusqlite with bundled SQLite | Small synchronous transaction boundary, consistent SQLite distribution, easy local tests. Reads execute away from Swift's main actor. |
| Bridge | UniFFI | Generated application records and Swift calls; no protocol objects leak across FFI. |
| Diagnostics | tracing-subscriber | CLI logging foundation; never log credentials, raw protocol traffic or message contents. |
| Push HTTP service | Axum | Small typed wake-relay surface with explicit body limits; independently deployed and not linked into the client. |
| Apple push transport | Hyper, rustls and ring | Token-authenticated APNs HTTP/2 with the same current TLS stack as IMAP and no OpenSSL runtime dependency. Notifications contain only an opaque subscription ID. |
| Account discovery | hickory-resolver, reqwest, quick-xml | RFC 6186 SRV lookup, HTTPS-only domain autoconfig, and bounded parsing of Thunderbird-compatible XML. Discovery does not authenticate. |

Network operations have bounded connect and command timeouts. Socket, DNS and
IDLE disconnects retry on a fresh connection with capped exponential backoff
and deterministic jitter; this also re-resolves addresses after Wi-Fi/cellular
or IPv4/IPv6 changes. Certificate, authentication and protocol failures fail
fast. IDLE is renewed within 25 minutes and falls back to periodic polling when
the server does not advertise the capability.

References:

- [async-imap](https://docs.rs/async-imap/latest/async_imap/)
- [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/)
- [UniFFI](https://mozilla.github.io/uniffi-rs/latest/)
- [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/)
- [Rustls](https://docs.rs/rustls/latest/rustls/)
- [Axum](https://docs.rs/axum/latest/axum/)
- [Apple APNs provider API](https://developer.apple.com/documentation/usernotifications/establishing-a-token-based-connection-to-apns)
- [RFC 6186](https://www.rfc-editor.org/rfc/rfc6186)
- [Thunderbird autoconfiguration format](https://developer.mozilla.org/en-US/docs/Mozilla/Thunderbird/Autoconfiguration/FileFormat/HowTo)

Do not infer Gmail/iCloud compatibility from a successful generic protocol build. Live provider verification needs test credentials and Google OAuth setup. SMTP selection is deliberately deferred until the sending slice; evaluate lettre and mail-builder then, including TLS, OAuth and attachment streaming.

The initial protocol tests exposed that async-imap 0.11.3 streaming FETCH/LIST helpers terminate on a tagged response without distinguishing OK from NO. Tern uses `run_command` and `read_response` with the library's parsed response types instead, and rejects failed or incomplete commands before committing a snapshot. It does not retain raw protocol transcripts. Regression tests cover a NO response after 100 fetched headers.
