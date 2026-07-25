---
name: speak
description: >-
  Convert text to speech on-device with the `speak` CLI (Supertonic-3 / ONNX
  Runtime) built in this repository. Use whenever the user wants to hear text
  read aloud, narrate or voice a message, generate spoken audio or a WAV/voice
  file from text, add text-to-speech (TTS), choose a voice, read a
  document/markdown/article aloud, speak the result of a task back to the user,
  or have the agent notify/alert/announce out loud when a task finishes or makes
  progress (for example "use speak to tell me when you're done", "read your last
  message to me", "announce progress with /speak") — even if they only say "say
  this out loud", "read this to me", or "make an audio version". Covers the
  critical output routing (stdout carries raw
  audio bytes, so use `--play` to make it audible or `--out` to save a file),
  the voice/speed/quality flags, streaming long documents, and the first-run
  model download.
---

# speak — on-device text-to-speech CLI

`speak` turns text into natural speech entirely on the local machine (no API
calls). It is a single compiled binary built on the Supertonic-3 model and ONNX
Runtime. Give it text, and it either plays the audio aloud, saves a WAV file, or
streams WAV bytes to another program.

Run it without installing anything via `bunx @nothumanwork/speak …` (see "How to
invoke it" below). The examples in this skill write just `speak` for brevity;
substitute `bunx @nothumanwork/speak` for that word unless the binary is already
on your `PATH`.

## The one rule that matters most: where does the audio go?

`speak` separates its two output streams: **stdout carries raw audio bytes, and
stderr carries every status and log line.** This is what lets a caller capture a
clean WAV while still seeing progress messages, but it is also the single most
common way to get a surprising result.

When you (an agent) run a command, stdout is almost always captured (a pipe), not
a live terminal. With no destination flag in that situation, `speak` assumes you
want the bytes and **streams a raw WAV into your captured stdout** — so nothing
plays, and you get binary data back instead of a tidy status line. Decide what
you want and pass the matching flag explicitly:

| You want to…                          | Command                                  |
|---------------------------------------|------------------------------------------|
| Play it aloud to the user now         | `speak "text" --play`                    |
| Save it to a WAV file                 | `speak "text" --out reply.wav`           |
| Save **and** play at the same time    | `speak "text" --out reply.wav --play`    |
| Pipe WAV into another program         | `speak "text" --stdout \| ffplay -`      |

`--play` forces audible playback even when stdout is captured, which is exactly
the agent case. `--out` always wins and writes the file regardless of the other
flags. Never rely on the bare `speak "text"` default from an agent context —
state the destination so the outcome is predictable. Playback needs a working
local audio output device (CoreAudio on macOS, ALSA/WASAPI elsewhere); if there
is none, prefer `--out`.

## How to invoke it

Prefer running it through Bun, which pulls the published binary from the npm
registry and caches it — no install step, and it works without the repository
checked out. Do not assume a `speak` binary is already on `PATH`:

```bash
bunx @nothumanwork/speak --list-voices
bunx @nothumanwork/speak "Your build finished." --play
```

The first run downloads a prebuilt `speak` binary (or builds it from source if
no release matches the platform), then later runs reuse the cache and are fast.
`npx -y @nothumanwork/speak …` is the npm-only equivalent. Use the registry
package name `@nothumanwork/speak`, not `bunx github:thehumanworks/speak`, which
404s because the GitHub repository is private (see `references/cli-reference.md`).

Only call the bare `speak …` form when the binary is genuinely installed on
`PATH` already (for example via `cargo install --path .`). As a last resort
inside a checkout, `cargo run --release -- "Hello." --play` builds and runs from
source.

## Quick start

```bash
# Play a sentence aloud
speak "Your build finished successfully." --play

# Save to a file with a specific voice
speak "Welcome aboard." --voice F1 --out welcome.wav

# Read text from stdin (pipe or a file)
cat notes.md | speak --play
speak "$(cat notes.md)" --out notes.wav

# Slower and higher quality
speak "Slow and clear." --voice F2 --speed 0.95 --steps 16 --out slow.wav

# See the available voices
speak --list-voices
```

## Speaking your reply back as a notification

A common ask is for the agent to announce task progress or its final answer out
loud — "tell me with speak when you're done", "read your last message to me". To
do this, run `speak` on the text you would otherwise just print, and always pass
`--play` so it is audible even though your stdout is captured:

```bash
bunx @nothumanwork/speak "Done — all 142 tests pass and the branch is pushed." --play
```

Two things make this reliable:

- **Keep the spoken version short.** A one or two sentence summary is a better
  spoken notification than reading a long markdown message verbatim. Write a
  brief line for the ear, even if your on-screen reply is longer.
- **Pass awkward text on stdin, not as a shell argument.** A message with quotes,
  backticks, or newlines is easy to mangle through shell quoting. Pipe it or use
  a heredoc instead:

  ```bash
  bunx @nothumanwork/speak --play <<'EOF'
  Finished the refactor. Two files changed, tests are green.
  EOF
  ```

`--play` blocks until playback finishes, so the command returns once the user has
heard it. Note that making this happen automatically on *every* turn is a harness
behavior, not something the skill can enforce: this skill makes the agent capable
of speaking a notification, but a recurring "always speak your last message" rule
needs a Stop hook (or a standing instruction you keep following) to fire it each
time.

## Voices

There are ten built-in voices: `M1`–`M5` (male) and `F1`–`F5` (female). The
default is `M1`. Each id is a distinct timbre and delivery, not an emotion
setting — Supertonic-3 has no separate emotion dial, so vary the feel by picking
a different voice and adjusting `--speed` and `--steps`. You can also pass a path
to a custom voice-style JSON file as the `--voice` value. Run `speak
--list-voices` to print the list.

## Shaping the speech

- `--voice / -v` — voice id (`M1`…`F5`) or a path to a custom style JSON. Default `M1`.
- `--speed` — speed factor; useful range `0.9`–`1.5`. Default `1.05`. Lower is slower and calmer.
- `--steps / -s` — denoising steps; higher is better quality but slower. Default `8`. Try `16` for narration you will keep.
- `--lang / -l` — language code. Default `en`. Many languages are supported (see the reference); use `na` for unknown text.
- `--gap` — pause in seconds inserted between paragraphs (inter-sentence and inter-clause pauses scale down from it). Default `0.3`.

Defaults are tuned for fast, good-enough speech. Reach for `--steps 16` and a
slightly lower `--speed` only when quality matters more than latency.

## Long documents stream automatically

Just pass the whole document — you do not need to split it yourself. `speak`
breaks long text into coherent chunks (sentences grouped up to ~240 characters,
never cut mid-word or on a decimal like `4.4`) and synthesizes them one at a
time, emitting each chunk's audio the moment it is ready. So **playback and
stdout streaming begin on the first chunk** (well under a second) instead of
after the entire document. Markdown is normalized before speaking: emphasis,
headings, horizontal rules, links, and bullet markers are reduced to their
spoken text, while ordered-list numbers and sentence structure are preserved.

Preview how a text will be segmented, without loading the model or synthesizing
anything, with `--dump-chunks`:

```bash
speak "$(cat long-document.md)" --dump-chunks
```

If the input normalizes to nothing speakable (only markup, code, or
punctuation), `speak` exits with an error rather than producing silence.

## First run downloads the model

On first use `speak` downloads the ~385 MB model from Hugging Face into a
per-user cache (`~/.cache/supertonic3`), reporting progress on stderr;
later runs start instantly. The first synthesis of a session can therefore take
a while — that is the download, not a hang.

- Override the cache location with `--model-dir <path>` or the
  `SUPERTONIC_CACHE_DIR` environment variable.
- Pass `--no-download` for offline or air-gapped use; `speak` then fails listing
  the missing `onnx/` model files instead of fetching anything (a missing
  per-voice style JSON surfaces separately, only when that voice is requested). A
  synthesis failing under `--no-download` with an unpopulated cache is expected,
  not a bug.

## Performance and device

CPU is the default and the recommendation: on Apple Silicon it already runs
faster than real time. GPU/CoreML support exists behind `--device auto` but
CoreML currently cannot run this model's dynamic-shape graph, so `auto` falls
back to CPU and ends up slower. Leave `--device` at its default unless you have a
specific reason.

## When you need more detail

- `references/cli-reference.md` — every flag with its default, the full list of
  supported language codes, exact output-routing rules, error/exit behavior, the
  npm/bunx invocation options, and more worked examples.
- `references/sdk.md` — the Rust `speak-core` SDK (`Engine`, `SynthesisRequest`,
  `Audio`, streaming via `speak_stream`). Read this only when the task is to call
  the library from Rust code or build a new front-end (Tauri app, web server)
  rather than invoke the CLI.
