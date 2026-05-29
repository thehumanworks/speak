# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`speak` is an on-device text-to-speech CLI built on Supertonic-3 and ONNX Runtime. Two crates:

- `src/main.rs` — the `speak` CLI, a thin wrapper over the SDK.
- `speak-core/` — the reusable SDK (`Engine`, `ModelLocator`, `SynthesisRequest`, `Audio`).

New front-ends (Tauri app, web server, etc.) should become new workspace members that depend on `speak-core`; they must not reach into the model plumbing directly.

## Hard invariants — breaking these ships bugs

- **stdout carries audio, stderr carries everything else.** When stdout is a pipe, the CLI streams raw WAV bytes to it. Every status, log, or diagnostic line must use `eprintln!`/`eprint!`. A stray `println!` on the synthesis path corrupts the WAV a caller is capturing. Gate optional diagnostics behind `--verbose`.
- **Do not edit or reformat `speak-core/src/helper.rs`.** It is vendored verbatim from `supertone-inc/supertonic` (MIT) and is exempt from clippy via `#[allow(dead_code, clippy::all)]` on the `mod helper;` line in `lib.rs`. Keep it in sync with upstream rather than changing it; don't let `cargo fmt` rewrite it.
- **The `_exit(0)` shutdown in `clean_exit` (`src/main.rs`) is deliberate.** It bypasses ONNX Runtime session destructors because dropping them on macOS hits a mutex-cleanup crash. Don't "fix" it into a normal drop/return.
- **Don't casually bump the pinned `ort`/`ort-sys` versions** (`=2.0.0-rc.12`, `tls-native`) in the workspace `Cargo.toml`. They match the vendored `helper.rs` API, and the download-binaries build script needs the TLS backend or linking fails.

## Cargo features

`default = ["coreml", "download"]`. `coreml` is the CoreML GPU execution provider (macOS); `download` auto-fetches the model from Hugging Face on first run. `--no-default-features` is the CPU-only, offline, portable build. Always validate both the default and `--no-default-features` configurations.

## Validation

Run `/validate` for the full matrix, or directly:

- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- repeat both with `--no-default-features`

`cargo fmt --check` reports drift in the vendored `helper.rs` (expected) — don't treat that as a failure or reformat it.

## Running the CLI

`speak` needs the model cache (~385 MB, auto-downloaded to `~/.cache/supertonic3` on first run). Override with `--model-dir` or `SUPERTONIC_CACHE_DIR`. Synthesis failing without a populated cache (under `--no-download`) is expected, not a bug.

## Git

Commit and push directly to `main`. Stage only the files you changed (`git add <paths>`, never `git add -A`) — there may be unrelated work-in-progress in the tree. Commit messages are short, lowercase, and imperative (e.g. "guard inference-backend message behind --verbose").
