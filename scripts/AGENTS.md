# Build and validation scripts

Inherit [root instructions](../AGENTS.md). These scripts implement the Devbox commands and checks invoked by Git hooks and CI. Read [quality policy](../docs/quality.md) before modifying them.

- Keep scripts runnable from their documented entry point regardless of the caller's working directory. Resolve repository paths from the script location and propagate command failures, including failures before `tee` in a pipeline.
- Use pinned Devbox tools and committed Cargo resolution. Preserve the native Xcode selection in `apple-toolchain.sh`: clear Nix's SDK overrides in the script process, then resolve the user's selected Xcode with native system tools. Do not change the system-wide Xcode selection.
- Generate UniFFI bindings from the compiled library with Cargo metadata available from `core/`. Keep generated files and validation logs in ignored directories.
- `check-swift.sh` needs a clean, verbose compiler log for SwiftLint analysis. Preserve fixture creation and cleanup, both test database environment variables, actual Swift tests, and analyzer execution. An incremental build log or an empty analyzer run is not equivalent verification.
- Fixture creation must use a new temporary database; never overwrite a user's mail store. Restrict cleanup to artifacts created by the script.
- Keep `devbox.json`, `.githooks/`, `.github/workflows/ci.yml` and documentation aligned when changing a check. Preserve failing exit statuses; unavailable security advisory data is not a passing audit.

Validate shell syntax for changed scripts, run affected entry points through Devbox, and exercise both language gates for changes to the shared validation path. Report hosted CI separately from local script execution.
