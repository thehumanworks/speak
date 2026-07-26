---
name: speak
description: >-
  Convert text to speech with the local `speak` CLI. Use when the user asks to
  hear text, generate a WAV, narrate content, announce progress, or play remote
  synthesis on an iPhone/iPad SSH client. Covers explicit destination routing,
  including `--play`, `--out`, `--stdout`, desktop SSH playback through
  `--play-stdin`, and the SSH tunnel/browser `--ios` path.
---

# speak — on-device text-to-speech

`speak` uses Supertonic-3 and ONNX Runtime locally. It can play through the
machine running the command, write a WAV, stream WAV bytes, or hand audio to an
iPhone/iPad browser through an SSH local forward.

Prefer the installed binary when present. Otherwise use:

```bash
bunx @nothumanwork/speak [OPTIONS] [TEXT]
```

## Always choose the destination explicitly

Output is mode-specific. `--stdout` writes only WAV bytes to stdout, while
`--ios` writes only the one-time browser URL. Status and diagnostics use stderr.
Agent commands normally capture stdout, so choose a destination explicitly.

| Intent | Command |
|---|---|
| Play on this machine | `speak --play "text"` |
| Save a WAV | `speak --out reply.wav "text"` |
| Save and play | `speak --out reply.wav --play "text"` |
| Pipe audio | `speak --stdout "text" \| ffplay -` |
| Play remote synthesis on this desktop | `ssh -T host 'speak --stdout "text"' \| speak --play-stdin` |
| Play from an iOS SSH client | `speak --ios "text"` |

Over SSH, `--play` means the **remote host's** audio device. It cannot address
the SSH client's speakers.

## Desktop over SSH

Use a non-PTY execution channel and play the canonical WAV on the local machine:

```bash
printf '%s' 'The remote task completed.' \
  | ssh -T user@host 'speak --stdout' \
  | speak --play-stdin
```

## iPhone/iPad over SSH

Configure the SSH client once:

```text
local  127.0.0.1:17820
remote 127.0.0.1:17820
```

Then run remotely:

```bash
speak --ios "The remote task is complete."
```

Tap the one-time localhost URL printed in the terminal. Keep the SSH connection
active while the browser loads the audio. Safari can require tapping Play.

When the forwarded local port is different:

```bash
speak --ios --ios-url http://127.0.0.1:8080 "Hello."
```

Do not expose `--ios-bind 0.0.0.0:...` to the public internet. A non-loopback
bind is only appropriate on a trusted private network/tailnet or behind an
authenticated HTTPS proxy.

## Agent notification workflow

1. Write the normal progress/final update first.
2. Speak only meaningful milestones and a concise final summary.
3. Use `--play` locally, the `--stdout | --play-stdin` pipeline for desktop
   SSH, or `--ios` for an iOS SSH session.
4. Pass shell-sensitive text via stdin.
5. If optional speech fails, report it and continue delivering the written work.

Local example:

```bash
speak --play --speed 1.1 <<'SPEAK_TEXT'
The implementation is complete and all checks pass.
SPEAK_TEXT
```

iOS SSH example:

```bash
speak --ios --speed 1.1 <<'SPEAK_TEXT'
The implementation is complete. Open the link to hear the summary.
SPEAK_TEXT
```

Never interpolate untrusted text into a shell command. Never speak secrets,
credentials, personal data, raw logs, long URLs, or dense code.

## Synthesis controls

- `--voice / -v`: `M1`–`M5`, `F1`–`F5`, or a custom style JSON. Default `M1`.
- `--speed`: useful range around `0.9`–`1.5`. Default `1.05`.
- `--steps / -s`: higher quality/latency. Default `8`; try `16` for saved narration.
- `--lang / -l`: language code. Default `en`; use `na` when unknown.
- `--gap`: paragraph pause. Default `0.3` seconds.
- `--device`: keep `cpu` unless testing another compatible model/backend.

## Long input

Pass the complete document. `speak` normalizes Markdown and splits prose into
coherent chunks without cutting words, decimals, common abbreviations, or list
markers. Local playback and stdout streaming start on the first chunk. `--ios`
creates a complete seekable WAV because mobile players commonly use content
length and byte ranges.

Preview segmentation without synthesis:

```bash
speak "$(cat document.md)" --dump-chunks
```

## First run and cache

The first synthesis may download roughly 385 MB into
`~/.cache/supertonic3`. Override with `--model-dir` or
`SUPERTONIC_CACHE_DIR`. Use `--no-download` when network access is forbidden and
expect a clear error if the cache is incomplete.

## References

- `references/cli-reference.md`: complete flags, routing, iOS setup, languages,
  cache behavior, and errors.
- `references/sdk.md`: Rust `speak-core` API and streaming callbacks.
