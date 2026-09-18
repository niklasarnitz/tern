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

References:

- [async-imap](https://docs.rs/async-imap/latest/async_imap/)
- [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/)
- [UniFFI](https://mozilla.github.io/uniffi-rs/latest/)
- [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/)
- [Rustls](https://docs.rs/rustls/latest/rustls/)

Do not infer Gmail/iCloud compatibility from a successful generic protocol build. Live provider verification needs test credentials and Google OAuth setup. SMTP selection is deliberately deferred until the sending slice; evaluate lettre and mail-builder then, including TLS, OAuth and attachment streaming.

The initial protocol tests exposed that async-imap 0.11.3 streaming FETCH/LIST helpers terminate on a tagged response without distinguishing OK from NO. Tern uses `run_command` and `read_response` with the library's parsed response types instead, and rejects failed or incomplete commands before committing a snapshot. It does not retain raw protocol transcripts. Regression tests cover a NO response after 100 fetched headers.
