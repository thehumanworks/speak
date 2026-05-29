---
name: validate
description: Run the full validation matrix for the speak workspace — clippy (deny warnings) and tests in both the default and --no-default-features configurations. Use before committing, after non-trivial changes, or when asked to verify the build.
---

# Validate the speak workspace

Run these four checks from the repo root, in order. Report each as pass/fail with the relevant output on failure. Do not stop at the first failure unless a build error blocks the rest — run all four so the user sees the full picture.

1. `cargo clippy --workspace --all-targets -- -D warnings`
2. `cargo test --workspace`
3. `cargo clippy --workspace --all-targets --no-default-features -- -D warnings`
4. `cargo test --workspace --no-default-features`

Steps 3 and 4 cover the CPU-only, offline, portable build (`coreml` and `download` features off), which is easy to break without noticing.

## Notes

- **Do not run `cargo fmt --check` as part of validation.** The vendored `speak-core/src/helper.rs` carries intentional formatting drift (it tracks upstream, not this crate's style), so the check always reports a diff. That is expected, not a failure.
- Clippy must be clean: the vendored `helper.rs` is already silenced via `#[allow(dead_code, clippy::all)]`, so any warning clippy reports is in first-party code and must be fixed (or, if truly upstream-vendored, scoped with a comment explaining why).
- If clippy or tests fail, fix the first-party code; never silence a first-party lint just to make the check pass.
