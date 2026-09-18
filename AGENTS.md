# Working on Tern

Read this file first, then the `AGENTS.md` in each directory you will change. Child files add local constraints; shared workflow and architecture rules live here. Commands below run from the checkout's repository root.

## Architecture and current scope

Tern is an offline-first Apple mail client with a portable Rust core. SQLite is the canonical local state. Swift calls the application API through generated UniFFI bindings; networking, MIME interpretation, synchronization and SQL belong in Rust.

The implemented slice is a CLI import of recent Inbox headers into SQLite plus a native macOS cache viewer. The Swift API currently reads accounts, mailboxes and bounded message pages. Repeated recent-header imports are not incremental synchronization. Message bodies, offline mutations, SMTP, Gmail OAuth, Keychain onboarding and an iOS app target remain future work.

```text
Apple SwiftUI -> UniFFI -> mail-core -> mail-db -> SQLite
Development CLI -> mail-sync -> mail-imap -> mail-mime
                            -> mail-db
mail-model supplies shared records across these boundaries.
```

| Area | Ownership |
| --- | --- |
| `core/crates/mail-model` | Portable records and exported value types |
| `core/crates/mail-db` | Schema migrations, persistence, identities and indexed pages |
| `core/crates/mail-imap` | Verified TLS, IMAP commands and typed protocol responses |
| `core/crates/mail-mime` | Normalization through an existing MIME parser |
| `core/crates/mail-sync` | Credential abstraction and protocol-to-store orchestration |
| `core/crates/mail-core` | Application facade, UniFFI generation entry point and development CLI |
| `apple` | Native SwiftUI presentation, selection, paging and lifecycle |
| `scripts`, `.githooks`, `.github/workflows` | Reproducible builds and required quality checks |

Keep these boundaries explicit. Expose application operations across FFI, rather than protocol commands or database handles. Keep secrets out of SQLite, logs, fixtures and commits; platform credential stores provide values through an abstraction. Preserve offline reads when networking fails. Use bounded database windows and FFI payloads rather than whole-mailbox state.

Read [architecture and milestones](docs/architecture.md) before changing ownership, persistence semantics or feature scope. Read [dependency decisions](docs/dependencies.md) before changing a protocol, MIME, TLS, database or bridge library. Read [validation evidence](docs/validation.md) when assessing readiness; dated test results are historical evidence, not proof that the current tree passes.

## Subagents and branches

Use cheaper capable subagents for bounded, independent work that can run alongside useful lead-agent work. Keep small or tightly coupled changes local; do not create coordination overhead just to delegate. Prefer the available lower-cost model for routine implementation and documentation; increase capability only when the task warrants it.

1. Inspect `git status`, current contracts and affected child instructions. Preserve unrelated changes. Agree on shared record/API changes before parallel edits.
2. Give each subagent a separate branch and worktree based on `master` (or an explicitly agreed dependency branch). Use descriptive names, such as `feature/mailbox-sync` or `docs/crate-guidance`. Create worktrees under ignored `.worktrees/`; never switch the branch in another agent's checkout.
3. Assign a small slice with an objective, owned files, dependencies, interface contract, exact checks, a checkable done condition and expected handoff evidence. One agent owns each file at a time. The lead owns shared manifests, lockfiles and generated-binding integration unless explicitly delegated.
4. Subagents make focused commits on their own branches and hand back the branch, commit IDs, changed behavior, checks/results and remaining limitations. They do not merge themselves into `master`.
5. The lead reads the complete branch diff against its agreed base, checks architecture/spec fit and verification evidence, and requests fixes before merging. Passing tests alone do not replace review.
6. Merge approved branches into `master` with a named merge commit (`git merge --no-ff <branch>`). Validate the integrated code and resolve cross-slice problems before declaring completion. Retain branches/worktrees until their work is safely committed and merged; inventory uncommitted files before cleanup.

Do not create user-facing tasks for internal delegation unless the user explicitly asks for a separate task.

## Commits

Commit frequently at coherent, reviewable boundaries: one behavior, fix or documentation concern per commit. Use concise human-readable subjects that explain the change, such as `Validate IMAP fetch completion status`. Keep follow-up fixes visible; do not rewrite existing history unless requested.

Stage owned paths explicitly and inspect the staged diff. Include relevant tests and documentation with the behavior they explain. Keep generated bindings, build outputs, local databases, credentials and `.devbox/` out of commits. Commit dependency/configuration changes with their corresponding lockfile updates. Keep hooks enabled; fix failures rather than bypassing them or weakening rules to obtain a passing result.

Check remotes before discussing pushes or hosted CI. A local merge does not imply publication, a hosted CI pass or server-side branch protection.

## Toolchain and verification

Use the repository's Devbox environment for Rust and pinned quality tools; do not introduce a separate rustup/global toolchain. `devbox.json` and lockfiles are the version/script authorities. Xcode supplies Swift and Apple SDKs. Use the Apple wrapper scripts: Nix SDK overrides otherwise prevent SourceKit from finding the Swift toolchain.

- `devbox run setup-hooks`: enable tracked hooks in a new clone.
- `devbox run check-rust`: formatting, strict Clippy, rustdoc, tests and dependency checks.
- `devbox run check-swift`: strict SwiftLint/SwiftFormat, generated bindings, Swift 6 compilation, bridge tests and unused-code analysis.
- `devbox run validate`: integrated required checks for the host platform.
- `devbox run bindings` / `devbox run macos`: regenerate bindings / run the cache viewer.

Read [quality policy](docs/quality.md) before changing checks or introducing lint exceptions. New Rust crates inherit workspace lints. Keep exceptions narrow and justified. Focus tests on observable failure modes and contracts. For documentation-only changes, verify paths, commands, scope and factual claims; existing commit hooks still run.

Completion reports identify the commits, actual checks run and remaining gates. Distinguish local tests, scripted-server tests, live-provider tests, visual/device checks and hosted CI. Never label the first slice as the completed public MVP.
