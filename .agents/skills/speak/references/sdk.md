# speak-core SDK reference (Rust)

Read this only when the task is to call the text-to-speech engine from Rust code
or build a new front-end (a Tauri app, a web server, a "read this webpage aloud"
tool) rather than invoke the `speak` CLI. The CLI is a thin wrapper over this
SDK; new front-ends should be new workspace members that depend on `speak-core`
and must not reach into the model plumbing directly.

## Public API surface

From `speak_core`:

- `Engine` — four ONNX sessions loaded once and reused across calls.
- `ModelLocator` — finds the model assets on disk.
- `SynthesisRequest` — parameters for one synthesis call (chainable setters).
- `Audio` — synthesized mono 32-bit float PCM plus its sample rate.
- `AudioChunk` — one streamed segment (audio + trailing gap + position).
- `Device` — `Cpu` (default) or `Auto` (GPU/CoreML with CPU fallback).
- `BUILTIN_VOICES` — the ten voice ids.
- `plan_chunks(text, gap) -> Vec<PlannedChunk>` — segment text without synthesizing.
- `is_valid_lang(&str)` / `AVAILABLE_LANGS` — language validation.
- `ensure_models(&locator)` — pre-fetch missing model files (requires the `download` feature).

## Loading the engine

```rust
use speak_core::{Device, Engine, ModelLocator, SynthesisRequest};

// Strict load from a populated cache; never touches the network:
let mut engine = Engine::load(ModelLocator::from_cache())?;

// Same, but on a chosen device:
let mut engine = Engine::load_with(ModelLocator::from_cache(), Device::Cpu)?;

// Download any missing files first, then load (requires the `download` feature):
let mut engine = Engine::load_or_download(ModelLocator::from_cache(), Device::Cpu)?;
```

`ModelLocator::from_cache()` resolves `$SUPERTONIC_CACHE_DIR` or
`~/.cache/supertonic3`; `ModelLocator::new(path)` uses an explicit base
directory. Inspect readiness before loading with `locator.is_available()` or
`locator.missing_files()`.

## Synthesizing all at once

`Engine::speak` returns the whole document as one `Audio`, with the planned
inter-chunk silences already concatenated in. Best when you are writing a file or
returning a complete buffer.

```rust
let audio = engine.speak(
    "F1",
    &SynthesisRequest::new("Hello from Rust.")
        .lang("en")     // default "en"
        .speed(1.0)     // default 1.05
        .steps(12)      // default 8
        .silence(0.3),  // default 0.3, the inter-paragraph gap
)?;

audio.write_wav("hello.wav")?;          // write a 16-bit PCM WAV file
let wav_bytes = audio.to_wav_bytes()?;  // in-memory WAV (HTTP body, IPC, ...)
let pcm: &[f32] = &audio.samples;       // raw mono f32 PCM
let seconds = audio.duration_secs();
```

## Streaming chunk by chunk

`Engine::speak_stream` synthesizes one chunk at a time and hands each chunk's
audio to a callback the moment it is ready, so a consumer can start playing or
sending audio while the rest is still generating. This is what makes
time-to-first-audio under a second on long documents.

```rust
engine.speak_stream("F1", &SynthesisRequest::new(long_text), |chunk| {
    // chunk.audio    : this segment's PCM (no trailing gap baked in)
    // chunk.gap_after: seconds of silence to play before the next chunk
    // chunk.text     : the normalized text that was spoken
    // chunk.index    : zero-based position
    // chunk.total    : number of chunks in the stream
    player.enqueue(&chunk.audio.samples);
    Ok(())   // returning Err stops the stream early and propagates the error
})?;
```

`plan_chunks(text, gap)` exposes the same segmentation without synthesizing if
you only need the chunk plan (for a preview, a progress bar, or your own
batching).

## Concurrency

`Engine::speak` and `speak_stream` take `&mut self` because ONNX Runtime sessions
run single-threaded per session. For a web server, put the engine behind a
`Mutex`, or keep a small pool of engines and check one out per request.

## Hard invariants when building on this crate

These are load-bearing; breaking them ships bugs:

- **stdout carries audio, stderr carries everything else.** Any front-end that
  streams audio to stdout must keep every status, log, and diagnostic line on
  stderr. A stray `println!` on the synthesis path corrupts a captured WAV.
- **Do not edit or reformat `speak-core/src/helper.rs`.** It is vendored verbatim
  from `supertone-inc/supertonic` (MIT). Keep it in sync with upstream rather
  than changing it, and do not let `cargo fmt` rewrite it.
- **The `_exit(0)` shutdown in the CLI's `clean_exit` is deliberate.** It bypasses
  ONNX Runtime session destructors because dropping them on macOS hits a
  mutex-cleanup crash. A new front-end on macOS that loads an `Engine` should
  expect the same and exit the process without dropping the engine where that
  matters.
- **Do not casually bump the pinned `ort` / `ort-sys` versions** in the workspace
  `Cargo.toml`; they match the vendored `helper.rs` API and the build's TLS
  backend.

## Cargo features

`default = ["coreml", "download"]`.

- `coreml` — the CoreML GPU execution provider (macOS).
- `download` — auto-fetch the model from Hugging Face on first use; gates
  `Engine::load_or_download` and `ensure_models`.
- `--no-default-features` — CPU-only, offline, portable build.

Validate both the default and `--no-default-features` configurations for any
change.
