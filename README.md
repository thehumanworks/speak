# speak

A compiled, on-device text-to-speech stack built on [Supertonic-3](https://github.com/supertone-inc/supertonic) and ONNX Runtime. It produces a small self-contained binary (`speak`) for AI agents, and a reusable SDK crate (`speak-core`) that a Tauri app or a Rust web server can depend on directly.

## Why this shape

The TTS pipeline is just four ONNX models plus a codepoint-based text front-end and per-voice style files. None of that is Python-specific, so it ports cleanly to a compiled language. ONNX Runtime is statically linked into the binary by the `ort` crate, so the executable is self-contained; the ~385 MB of model weights are loaded from a cache directory at runtime rather than baked in.

```
speak/
  Cargo.toml      # workspace root + the `speak` binary package
  src/main.rs     # the CLI: args, stdin, file/stdout/playback
  speak-core/     # the SDK: load models, synthesize, return audio (UI-agnostic)
    src/lib.rs      # public API: Engine, ModelLocator, SynthesisRequest, Audio
    src/helper.rs   # vendored upstream pipeline (text front-end + 4-model chain)
```

The CLI is a thin layer over the SDK. New front-ends (Tauri, web server, a webpage reader) become new workspace members that depend on `speak-core` — they never touch the model plumbing.

## Install

Install the `speak` command system-wide (into `~/.cargo/bin`) straight from a checkout:

```bash
cargo install --path .
```

The first install downloads and statically links ONNX Runtime, so it takes a few minutes; later installs are fast. Make sure `~/.cargo/bin` is on your `PATH` (it is by default with a standard Rust install), then `speak` is available everywhere:

```bash
speak --list-voices
```

The first time you run `speak`, it downloads the ~385 MB model from Hugging Face into a per-user cache (see [Models](#models)), so there is no separate setup step.

For a CPU-only, offline, fully portable binary (no CoreML, no auto-download), disable the default features:

```bash
cargo install --path . --no-default-features
```

The default features are `coreml` (GPU execution provider) and `download` (fetch the model on first run); you can enable them individually, e.g. `--no-default-features --features download`.

To install from a git remote once this is published, name the binary crate:

```bash
cargo install --git <repo-url> speak
```

## Build (without installing)

```bash
cargo build --release      # first build downloads & links ONNX Runtime
# binary: target/release/speak
```

## Models

On first run `speak` downloads the model from the [`Supertone/supertonic-3`](https://huggingface.co/Supertone/supertonic-3) repository on Hugging Face into a per-user cache, reporting progress on stderr. The download is pinned to a specific immutable commit, so every build fetches byte-identical weights; bumping the version is a one-line change to `MODEL_REVISION` in `speak-core/src/download.rs`. Only missing files are fetched, each is written atomically (a partial download never looks complete), and concurrent first runs are serialized by a lock file so two processes never download the same files at once. Subsequent runs start instantly. The cache layout is:

```
~/.cache/supertonic3/
  onnx/{duration_predictor,text_encoder,vector_estimator,vocoder}.onnx
  onnx/{tts.json,unicode_indexer.json}
  voice_styles/{M1..M5,F1..F5}.json
```

Override the base directory with `--model-dir`, or the `SUPERTONIC_CACHE_DIR` environment variable. Pass `--no-download` to disable fetching and fail if the cache is incomplete (useful for offline or air-gapped use, where you would populate the directory yourself, e.g. `git clone https://huggingface.co/Supertone/supertonic-3` into it). If files are missing and downloading is off, `speak` prints exactly which ones.

## CLI usage

```bash
# Speak aloud (default when stdout is a terminal)
speak "Hello from a compiled binary."

# Save to a WAV file
speak "Save me to disk." --voice F1 --out hello.wav

# Read text from stdin
echo "Piped in from stdin." | speak --out out.wav

# Stream WAV to stdout (e.g. for an agent to capture)
speak "Pipe me." --stdout > out.wav

# Force playback even when stdout is captured (e.g. invoked by an agent)
speak "Heads up." --play

# Female voice, slower, higher quality
speak "Slow and clear." -v F2 --speed 0.95 --steps 16 -o slow.wav

# List built-in voices
speak --list-voices
```

Flags: `-v/--voice` (M1..M5, F1..F5, or a custom style JSON path), `-o/--out`, `--stdout`, `--play`, `-l/--lang`, `-s/--steps`, `--speed`, `--gap`, `--device`, `--model-dir`, `--no-download`, `--dump-chunks`.

"Tone" variety comes from picking among the ten voices plus `--speed` / `--steps`; Supertonic-3 has no separate emotion dial.

By default the destination is chosen automatically: `--out` writes a file, otherwise a non-terminal stdout (a pipe or redirect) streams WAV bytes and a real terminal plays aloud. Agents usually run with stdout captured, which would stream bytes; pass `--play` to force audible playback regardless, or `--stdout` to force byte streaming on a terminal. Playback uses the host machine's audio output via [`cpal`](https://crates.io/crates/cpal) (CoreAudio on macOS, ALSA/WASAPI elsewhere), so it needs an active local audio session.

## Streaming long documents

Long inputs are not synthesized in one shot. `speak` splits the text into coherent chunks — sentences grouped up to ~240 characters, never cut mid-word, never split on decimals/clause references (`4.4`, `5.2.1`), abbreviations, or list markers — and synthesizes them one at a time. Because each chunk's audio is emitted as soon as it is ready, **playback and stdout streaming begin on the first chunk** instead of after the whole document. On a ~3.9-minute document this cuts time-to-first-audio from ~40 s to under 1 s; synthesis (several times faster than real time) then stays ahead of playback so it sounds gapless.

Markdown is normalized before synthesis: emphasis (`**`, `*`, `` ` ``), headings (`#`), horizontal rules, links, and bullets are reduced to their spoken text, while ordered-list numbers and sentence structure are preserved. Each chunk is a complete unit ending in punctuation, which matters because the model derives intonation from the chunk alone — there is no cross-chunk context. Pauses scale with the boundary: short between clauses, a beat between sentences, and a longer rest between paragraphs (tune the paragraph pause with `--gap SECONDS`, default `0.3`).

When streaming a WAV to stdout, a header is written up front and PCM is flushed per chunk so a downstream player (`speak … --stdout | ffplay -`) hears audio as it arrives. Redirecting to a file (`speak … --stdout > out.wav`) seeks back and patches the real sizes into the header, producing a fully valid WAV; a true pipe gets the conventional streaming sentinel length.

Inspect the segmentation for any text without synthesizing it:

```bash
speak "$(cat long-document.md)" --dump-chunks
```

## Device / GPU

The default is CPU, which on Apple Silicon already runs faster than real-time (about 7.5 s of audio in ~1.7 s). GPU support is built in but opt-in:

```bash
speak "…" --device auto -o out.wav   # try CoreML (GPU + Neural Engine), fall back to CPU
```

The full CoreML execution-provider path is wired up (`speak-core`'s `coreml` feature, on by default in the build), with automatic CPU fallback for unsupported ops or init failures. In practice, however, CoreML cannot build an execution plan for Supertonic-3's dynamic-shape, flow-matching graph (it errors with code -7), so `--device auto` falls back to CPU and ends up ~3.4x slower than plain CPU because of the failed attempt. CPU is therefore the recommended and default device for this model. `--device auto` remains useful if you swap in a CoreML-compatible model or a future ONNX Runtime improves dynamic-shape support. For a CPU-only, fully portable binary, install or build with `--no-default-features`.

## SDK usage

```rust
use speak_core::{Device, Engine, ModelLocator, SynthesisRequest};

// Downloads the model on first use (requires the `download` feature):
let mut engine = Engine::load_or_download(ModelLocator::from_cache(), Device::Cpu)?;
// Or load strictly from a populated cache, never touching the network:
//   let mut engine = Engine::load(ModelLocator::from_cache())?;

let audio = engine.speak("F1", &SynthesisRequest::new("Hello.").speed(1.0).steps(12))?;
audio.write_wav("hello.wav")?;          // file
let wav_bytes = audio.to_wav_bytes()?;  // in-memory WAV (HTTP body, IPC, ...)
let pcm = &audio.samples;               // raw mono f32 PCM
```

`Engine::speak` returns the whole document at once. For long text, `Engine::speak_stream` synthesizes chunk by chunk and hands each chunk's audio to a callback the moment it is ready, so a consumer can start playing or sending audio while the rest is still being generated:

```rust
engine.speak_stream("F1", &SynthesisRequest::new(long_text), |chunk| {
    // chunk.audio is this segment's PCM; chunk.gap_after is the silence (s) to
    // play before the next chunk; chunk.index / chunk.total track progress.
    player.enqueue(&chunk.audio.samples);
    Ok(())  // returning Err stops the stream early
})?;
```

`speak_core::plan_chunks(text, gap)` exposes the same segmentation without synthesizing, if you only need the chunk plan. `Engine::speak` takes `&mut self` (ONNX sessions are single-threaded). For a web server, put the engine behind a `Mutex` or keep a small pool and check one out per request. The `download` feature also exposes `speak_core::ensure_models(&locator)` if you want to pre-fetch without loading.

### Example downstream members (future)

- **Tauri**: a `speak-tauri` member exposes a command that calls `engine.speak(...)` and returns `to_wav_bytes()` to the webview, or plays via the OS.
- **Web server**: a `speak-web` member (axum/actix) holds a pooled `Engine` and serves `POST /tts` returning `audio/wav`.
- **Read a webpage aloud**: a `speak-readability` member fetches a URL, extracts article text, splits it into requests, and feeds them to the same `Engine`. Only the text source is new; synthesis is unchanged.

## Licensing

`speak-core/src/helper.rs` is vendored from `supertone-inc/supertonic` (MIT-licensed sample code). The Supertonic-3 model weights are under the OpenRAIL-M license; review it before redistributing audio or models.
