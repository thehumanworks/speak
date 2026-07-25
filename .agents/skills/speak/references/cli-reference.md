# speak CLI reference

Complete reference for the `speak` command. Read `SKILL.md` first for the common
workflow; this file is the exhaustive lookup for flags, languages, output
routing, and error behavior.

## Synopsis

```
speak [OPTIONS] [TEXT]
```

The recommended way to run it without installing anything is `bunx
@nothumanwork/speak [OPTIONS] [TEXT]` (or `npx -y @nothumanwork/speak …`). The
examples in this file write just `speak` for brevity; substitute the `bunx`
form unless the binary is already installed on `PATH`. See "Running without a
local install" below.

`TEXT` is the text to speak. If omitted, `speak` reads the text from standard
input, so both of these work:

```bash
speak "Spoken from an argument."
echo "Spoken from stdin." | speak --play
speak --out doc.wav < document.txt
```

Leading and trailing whitespace is trimmed. Empty input (no argument and empty
stdin) is an error.

## Flags

| Flag | Default | Meaning |
|------|---------|---------|
| `-v, --voice <VOICE>` | `M1` | Built-in voice id (`M1`–`M5` male, `F1`–`F5` female, case-insensitive) or a path to a custom voice-style JSON file. |
| `-o, --out <PATH>` | — | Write a 16-bit PCM WAV file to `PATH`. Always takes precedence over `--stdout`. Combine with `--play` to also play aloud. |
| `--stdout` | off | Stream WAV bytes to stdout, even on a terminal. Ignored when `--out` is set. |
| `--play` | off | Play the audio aloud, forcing playback even when stdout is captured (the agent/pipe case). Can be combined with `--out` or `--stdout` to play *and* save/stream. |
| `-l, --lang <LANG>` | `en` | Language code (see list below). Use `na` for unknown text. |
| `-s, --steps <N>` | `8` | Denoising steps; higher is better quality but slower. |
| `--speed <FACTOR>` | `1.05` | Speech speed factor; `0.9`–`1.5` is the useful range. |
| `--gap <SECONDS>` | `0.3` | Pause between paragraphs; inter-sentence and inter-clause pauses scale down from it. |
| `--device <cpu\|auto>` | `cpu` | `cpu` forces CPU. `auto` tries GPU/CoreML with CPU fallback (currently falls back to CPU for this model, so it is slower). |
| `--model-dir <PATH>` | `$SUPERTONIC_CACHE_DIR` or `~/.cache/supertonic3` | Model base directory containing `onnx/` and `voice_styles/`. |
| `--no-download` | off | Do not download missing model files; fail if the cache is incomplete. |
| `--list-voices` | — | Print the built-in voices and exit (no synthesis). |
| `--dump-chunks` | — | Print how the text would be split into streaming chunks (index, char count, trailing gap, text) and exit, without loading the model. |
| `--verbose` | off | Print extra diagnostics to stderr (inference backend, time-to-first-audio). |
| `--version` | — | Print the version and exit. |
| `-h, --help` | — | Print help and exit. |

## Output routing (where the audio goes)

`speak` chooses a single destination from `--out`, `--stdout`, `--play`, and
whether stdout is a real terminal. The precedence is:

1. `--out PATH` is set → **write the WAV file** (wins over everything). If
   `--play` is also set, play aloud while writing.
2. Otherwise `--stdout` is set → **stream WAV bytes to stdout** (even on a
   terminal). If `--play` is also set, play aloud while streaming.
3. Otherwise `--play` is set → **play aloud**, even when stdout is captured.
4. Otherwise, no destination flag:
   - stdout is **not** a terminal (a pipe or captured by an agent) → **stream
     WAV bytes to stdout**.
   - stdout **is** a terminal → **play aloud**.

The practical takeaways:

- **stdout is audio; stderr is everything else.** Status, progress, and the
  "Saved N.NNs of audio to …" line all go to stderr, so capturing stdout yields a
  clean WAV.
- **From an agent, always pass an explicit destination** (`--play`, `--out`, or
  `--stdout`). The bare default in a captured-stdout context streams raw bytes,
  which is rarely what you want when the intent was to play audio.
- Streaming to stdout writes a WAV header up front and flushes PCM per chunk, so
  a downstream player hears audio as it arrives. Redirecting that stream to a
  file (`speak … --stdout > out.wav`) seeks back and patches the real sizes into
  the header, producing a fully valid WAV; a true pipe gets the conventional
  streaming sentinel length.

## Supported languages

`--lang` accepts these codes:

```
en  ko  ja  ar  bg  cs  da  de  el  es  et  fi  fr  hi  hr  hu  id  it
lt  lv  nl  pl  pt  ro  ru  sk  sl  sv  tr  uk  vi  na
```

`na` means "not applicable / unknown" — use it when the language of the text is
unknown. An unsupported code is an error that lists the valid codes.

## Voices

Ten built-in voices, all present once the model cache is populated:

```
M1 M2 M3 M4 M5   (male)
F1 F2 F3 F4 F5   (female)
```

Ids are case-insensitive (`f1` resolves to `F1`). Any `--voice` value that is not
a built-in id is treated as a filesystem path to a custom voice-style JSON file;
if neither a built-in id nor an existing file matches, `speak` errors. There is
no emotion parameter — vary delivery via voice choice, `--speed`, and `--steps`.

## Models and the cache

The cache layout under the base directory is:

```
<base>/
  onnx/{duration_predictor,text_encoder,vector_estimator,vocoder}.onnx
  onnx/{tts.json,unicode_indexer.json}
  voice_styles/{M1..M5,F1..F5}.json
```

The base directory is resolved as: `--model-dir` if given, else
`$SUPERTONIC_CACHE_DIR` if set, else `~/.cache/supertonic3`. On first run (with
downloading enabled, the default) missing files are fetched from the
`Supertone/supertonic-3` Hugging Face repository, pinned to an immutable commit
so every machine gets byte-identical weights. Only missing files are fetched,
each is written atomically, and concurrent first runs are serialized by a lock
file. With `--no-download`, missing model files cause an error that names which
of the required `onnx/` files are absent (a missing `voice_styles/*.json` is not
checked at load time — it instead errors when that voice is requested); populate
the directory yourself (for example,
`git clone https://huggingface.co/Supertone/supertonic-3` into it) for offline
use.

## Error and exit behavior

- Exits non-zero with a message on stderr for: empty input, an input that
  normalizes to nothing speakable (only markup/code/punctuation), an unsupported
  `--lang`, an unknown `--voice`, a missing model cache under `--no-download`, or
  no available audio output device when playback was requested.
- `--list-voices`, `--dump-chunks`, `--version`, and `--help` all exit `0`
  without synthesizing.

## Running without a local install (npm / Bun)

The package `@nothumanwork/speak` is published on the npm registry, so the
preferred install-free invocation is through the registry (it fetches a prebuilt
`speak` binary, or builds from source if no release matches the platform, and
caches it):

```bash
# recommended: registry package via Bun
bunx @nothumanwork/speak --list-voices

# npm-only equivalent
npx -y @nothumanwork/speak --list-voices
```

Avoid the GitHub-tarball form `bunx github:thehumanworks/speak`: Bun uses an
unauthenticated tarball API that returns 404 because the repository is private.
`npx -y github:thehumanworks/speak` does work (npm clones over git with your
credentials) but is not needed now that the registry package exists. Fast release
downloads (used by the postinstall) need `GITHUB_TOKEN` / `SPEAK_GITHUB_TOKEN`
(with `contents:read`) or a signed-in `gh`. Overrides: `SPEAK_VERSION=v0.1.0`
pins a tag, `SPEAK_REPO=owner/repo` selects another repository,
`SPEAK_NPM_SKIP_DOWNLOAD=1` skips the postinstall.

## Worked examples

```bash
# Speak a status update aloud to the user
speak "Tests passed: 142 of 142." --play

# Save a greeting in a female voice at higher quality
speak "Welcome back." --voice F3 --steps 16 --out greeting.wav

# Narrate a markdown document, slower, with longer paragraph pauses
speak "$(cat report.md)" --voice F1 --speed 0.97 --gap 0.5 --play

# Pipe live audio into a player
speak "Streaming straight to a player." --stdout | ffplay -autoexit -nodisp -

# Inspect segmentation only (no model load, no audio)
speak "$(cat report.md)" --dump-chunks

# Offline: fail loudly if the cache is incomplete instead of downloading
speak "Offline test." --no-download --out out.wav

# Diagnostics: show the backend and time-to-first-audio
speak "Diagnostics." --play --verbose
```
