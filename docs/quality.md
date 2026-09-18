# Required quality gates

Run `devbox run validate` before submitting changes. Run `devbox run setup-hooks` once per clone to enable tracked local hooks. CI checks the committed tree; local hooks check the working tree, so keep unrelated unfinished work isolated in branches/worktrees.

## Rust

- `cargo fmt --check` enforces the shared formatter.
- Clippy checks every crate, target, and feature with warnings as errors, including the `all` and `pedantic` groups.
- Handwritten unsafe Rust is forbidden. Ignored must-use results, `unwrap`, `expect`, explicit panic, debug macros, TODO and unimplemented stubs are rejected in production.
- Test-only `unwrap` allowances must be scoped to test modules/files. Other allowances need a local explanation; never disable a group to make a check green.
- Rustdoc warnings fail the build. All workspace tests must pass.
- `cargo audit --deny warnings` checks the lockfile against RustSec. `cargo deny` rejects unapproved licenses, unknown registries/git sources and wildcard dependencies. Multiple dependency versions remain diagnostic because upstream crates can legitimately require incompatible versions. Advisory exceptions are empty and require explicit review to add.
- Dependency checks need network access to refresh advisory data. An unavailable advisory database is a failure, not a passing security check.

## Swift

- Swift 6 language mode enforces complete concurrency checking. Compiler warnings fail the build.
- SwiftLint runs in strict mode with its default rules and selected correctness/readability opt-ins. Force casts, force tries and force unwraps fail.
- SwiftFormat checks source and the package manifest without changing files in CI.
- SwiftLint's analyzer checks unused declarations/imports from a clean verbose compiler log. A formatting-only check is not a substitute for compilation or analysis.
- UniFFI-generated code is excluded from style/analyzer checks. It still compiles under Swift 6 and warnings-as-errors. Fix the generator/configuration for generated-code issues rather than editing generated files.

`devbox run check-rust` runs the Rust gates; `devbox run check-swift` runs the Apple gates. `devbox run validate` runs both on Apple Silicon macOS and Rust checks on Linux. Xcode supplies Swift and Apple SDKs; Devbox pins the linter/formatter versions. Apple scripts clear Nix SDK overrides and select the active Xcode so SourceKit and SwiftPM use the real Swift toolchain. The initial Apple CI runner is Apple Silicon to match the supported Devbox package set.

Tracked pre-commit hooks run formatting, Clippy, SwiftLint and SwiftFormat; pre-push runs full validation. These are useful local checks but can be bypassed by Git flags. Required CI status checks must also be enabled in the hosting service's branch protection when a remote repository is configured.
