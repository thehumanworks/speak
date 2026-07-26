# CLAUDE.md

This file provides guidance to coding agents working in this repository.

## What this is

`speak` is an on-device text-to-speech CLI built on Supertonic-3 and ONNX Runtime.

- `src/main.rs` — CLI parsing and destination routing.
- `src/player.rs` — local real-time playback.
- `src/wavstream.rs` — incremental WAV output.
- `src/wavinput.rs` — incremental playback of WAV received on stdin.
- `src/ios.rs` — tokenised, short-lived browser playback for iOS SSH clients.
- `speak-core/` — reusable synthesis SDK (`Engine`, `ModelLocator`, `SynthesisRequest`, `Audio`).

Transport/front-end code must depend on `speak-core`; it must not duplicate or reach into the model plumbing.

## Hard invariants

- **stdout is protocol output, selected by mode.** In `--stdout` mode it contains only WAV bytes. In `--ios` mode it contains only the tappable playback URL. All status, progress, warnings, and diagnostics use stderr. A stray `println!` on the WAV path corrupts callers.
- **`--play` means the machine executing `speak`.** It never implies SSH audio forwarding. A PTY-backed SSH session without an explicit destination must remain an actionable error.
- **The iOS server is private by default.** Keep the default listener on loopback, retain the random bearer path, no-store headers, strict URL validation, range support, bounded request parsing, and expiry. Public plain HTTP exposure must never be the recommended path.
- **Do not edit or reformat `speak-core/src/helper.rs`.** It is vendored from `supertone-inc/supertonic` and exempted through `#[allow(dead_code, clippy::all)]` in `lib.rs`.
- **The `_exit(0)` shutdown in `clean_exit` is deliberate.** It bypasses an ONNX Runtime destructor crash seen on macOS.
- **Do not casually bump `ort`/`ort-sys`.** Their exact versions match the vendored helper API and binary-download setup.

## Cargo features

`default = ["coreml", "download"]`. `coreml` enables the CoreML execution provider; `download` fetches model files on first use. `--no-default-features` is CPU-only and offline unless `--features download` is added.

## Validation

Validate both default and portable configurations when practical:

```bash
cargo test --workspace --locked
cargo test --workspace --locked --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
```

Do not run a whole-workspace formatter that rewrites `speak-core/src/helper.rs`. Format/check explicitly changed non-vendored Rust files instead.

The iOS HTTP tests use loopback and synthetic bytes; they must not load the TTS model or require an audio device. `wavinput` parser tests likewise must stay device-independent.

## Models

The model cache is approximately 385 MB and defaults to `~/.cache/supertonic3`. Override with `--model-dir` or `SUPERTONIC_CACHE_DIR`. Missing models under `--no-download` are expected to fail with an actionable error.

## Git

Stage only files changed for the task. Keep commits small, imperative, and scoped. Do not mix generated model files, build products, or unrelated working-tree changes into a commit.
