# `speak` CLI reference

Complete reference for the `speak` command. Read the parent `SKILL.md` first for
the common workflow.

## Synopsis

```text
speak [OPTIONS] [TEXT]
speak --play-stdin [--verbose]
```

`TEXT` is spoken directly. When omitted in synthesis modes, `speak` reads UTF-8
text from stdin. `--play-stdin` instead expects the canonical WAV stream emitted
by `speak --stdout` and performs no synthesis.

## Destination flags

| Flag | Default | Meaning |
|---|---:|---|
| `-o, --out <PATH>` | — | Write a complete 16-bit PCM WAV. Combine with `--play` to save and play locally. |
| `--stdout` | off | Stream WAV bytes to stdout, flushing each synthesized chunk. Combine with `--play` to also play locally. |
| `--play` | off | Play through the default output device on the machine running `speak`. Over SSH, that is the remote host. |
| `--play-stdin` | off | Incrementally validate and play the canonical mono PCM16 WAV received on stdin. Does not load the model. |
| `--ios` | off | Serve a short-lived browser player for an iPhone/iPad SSH client. Conflicts with `--out`, `--stdout`, and `--play`. |
| `--ios-bind <ADDR>` | `127.0.0.1:17820` | Remote listen address for `--ios`; requires `--ios`. |
| `--ios-url <ORIGIN>` | bind origin | Origin printed in the one-time URL. It must be `http://` or `https://` with no path, query, fragment, or whitespace. |
| `--ios-timeout <SECONDS>` | `300` | Browser-link lifetime. Valid range: 1–86,400; requires `--ios`. |

### Automatic destination routing

Destination precedence is:

1. `--play-stdin` → consume WAV stdin; no model load.
2. `--out PATH` → file; `--play` adds local playback.
3. `--ios` → temporary browser player.
4. `--stdout` → WAV stdout; `--play` adds local playback.
5. `--play` → local/default audio device.
6. No destination and stdout is not a terminal → WAV stdout.
7. No destination in an interactive SSH terminal → actionable error.
8. No destination in a local terminal → local/default audio device.

Output is mode-specific. `--stdout` writes only WAV bytes to stdout, while
`--ios` writes only the tappable playback URL. Status, progress, diagnostics,
paths, and warnings use stderr.

## Desktop SSH playback

```bash
printf '%s' 'Remote synthesis.' \
  | ssh -T user@host 'speak --stdout' \
  | speak --play-stdin
```

The local player accepts complete or unknown-length canonical WAV headers and
streams samples to the local audio device. Use a non-PTY SSH execution channel.

## iPhone/iPad SSH playback

SSH terminal channels cannot redirect the remote host's audio device into an iOS
SSH application. `--ios` provides a client-independent hand-off over an SSH local
forward.

Configure the iOS SSH client with:

```text
local  127.0.0.1:17820
remote 127.0.0.1:17820
```

OpenSSH equivalent:

```bash
ssh -L 17820:127.0.0.1:17820 user@host
```

Then run in the remote shell:

```bash
speak --ios "Hello on iOS."
```

Tap the printed `http://127.0.0.1:17820/<random-token>` URL. The endpoint:

- binds to loopback by default;
- uses a random 128-bit bearer path;
- serves a complete WAV with `HEAD` and single-byte-range support;
- sets no-store and browser hardening headers;
- exposes no directory or file-system content; and
- exits after playback/fetch or the configured timeout.

Safari can reject audible autoplay; the page always exposes native playback
controls. Keep the SSH tunnel active while the browser fetches audio.

When the iOS local port differs from the remote port:

```bash
# Client forward: local 8080 -> remote 127.0.0.1:17820
speak --ios --ios-url http://127.0.0.1:8080 "Hello."
```

If an external browser causes the SSH app to suspend its tunnel, use the SSH
client's in-app browser/split view, or expose the endpoint only on a trusted
private network/tailnet:

```bash
speak --ios \
  --ios-bind 0.0.0.0:17820 \
  --ios-url http://100.64.0.10:17820 \
  "Hello privately."
```

Do not expose the plain-HTTP listener directly to the public internet. Use the
SSH tunnel, a private network, or an authenticated HTTPS reverse proxy.

## Synthesis flags

| Flag | Default | Meaning |
|---|---:|---|
| `-v, --voice <VOICE>` | `M1` | `M1`–`M5`, `F1`–`F5`, case-insensitive, or a custom voice-style JSON path. |
| `-l, --lang <LANG>` | `en` | Language code. Use `na` for unknown/mixed input. |
| `-s, --steps <N>` | `8` | Denoising steps; higher can improve quality at greater latency. |
| `--speed <FACTOR>` | `1.05` | Speech speed; approximately `0.9`–`1.5` is useful. |
| `--gap <SECONDS>` | `0.3` | Paragraph pause; sentence/clause pauses scale from it. |
| `--device <cpu\|auto>` | `cpu` | CPU, or CoreML attempt with CPU fallback on supported builds. |
| `--model-dir <PATH>` | cache | Base directory containing `onnx/` and `voice_styles/`. |
| `--no-download` | off | Fail when required assets are absent instead of downloading them. |
| `--list-voices` | — | Print built-in voices and exit without loading models. |
| `--dump-chunks` | — | Print normalized long-text chunks and exit without synthesis. |
| `--verbose` | off | Print backend and timing diagnostics to stderr. |
| `--version` | — | Print the binary version. |
| `-h, --help` | — | Print help. |

## Supported languages

```text
en  ko  ja  ar  bg  cs  da  de  el  es  et  fi  fr  hi  hr  hu  id  it
lt  lv  nl  pl  pt  ro  ru  sk  sl  sv  tr  uk  vi  na
```

## Voices

```text
M1 M2 M3 M4 M5
F1 F2 F3 F4 F5
```

The voices represent different timbres/deliveries, not explicit emotions.
Adjust voice, speed, and steps to shape delivery.

## Model cache

Resolution order:

1. `--model-dir <PATH>`
2. `SUPERTONIC_CACHE_DIR`
3. `~/.cache/supertonic3`

Expected layout:

```text
<base>/
  onnx/{duration_predictor,text_encoder,vector_estimator,vocoder}.onnx
  onnx/{tts.json,unicode_indexer.json}
  voice_styles/{M1..M5,F1..F5}.json
```

With the `download` feature, missing assets are fetched from a pinned immutable
Hugging Face revision. Files are written atomically, and concurrent downloads
are serialized. `--no-download` turns missing required ONNX assets into an error.

## Long-text behavior

Long input is normalized and split into coherent chunks. Playback and stdout
streaming start with the first chunk. `--ios` instead synthesizes a complete WAV
because iOS media consumers commonly need a known content length and byte-range
access.

## Errors and exit behavior

Non-zero exits include:

- empty or non-speakable input;
- unsupported language or voice;
- missing model files with downloads disabled;
- unavailable local audio device when local playback was requested;
- interactive SSH with no explicit destination;
- invalid/conflicting iOS options;
- inability to bind or serve the temporary browser endpoint;
- malformed, truncated, or unsupported WAV passed to `--play-stdin`; and
- audio output/stream failures.

The temporary iOS endpoint expiring normally is reported on stderr and exits
successfully; it is a bounded hand-off, not a daemon.

## Examples

```bash
speak "Tests passed: 142 of 142." --play
speak "Welcome back." --voice F3 --steps 16 --out greeting.wav
speak "$(cat report.md)" --voice F1 --speed 0.97 --gap 0.5 --play
speak "Streaming." --stdout | ffplay -autoexit -nodisp -
printf '%s' 'Remote.' | ssh -T host 'speak --stdout' | speak --play-stdin
speak "Remote synthesis, iOS playback." --ios
speak "$(cat report.md)" --dump-chunks
speak "Offline test." --no-download --out out.wav
speak "Diagnostics." --play --verbose
```
