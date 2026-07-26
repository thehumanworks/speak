---
name: speak
description: Use the local `speak` text-to-speech CLI for concise audible progress updates and final summaries, including desktop SSH playback and iPhone/iPad browser hand-off.
---

# Speak

Use audible speech as a companion to the written response, never as its
replacement.

## Workflow

1. Verify `speak` with `command -v speak` when necessary.
2. Speak only meaningful milestones and a concise final outcome.
3. Write the normal update first, then invoke speech.
4. Choose the destination explicitly:
   - local machine: `--play`
   - desktop SSH: remote `--stdout`, piped into local `--play-stdin`
   - iPhone/iPad SSH session: `--ios` with a configured local forward
5. Pass shell-sensitive text through stdin.
6. If optional speech fails, report it briefly and continue.

Local playback:

```sh
speak --play --speed 1.1 <<'SPEAK_TEXT'
The short conversational update goes here.
SPEAK_TEXT
```

Desktop SSH playback uses a binary-transparent non-PTY channel:

```sh
printf '%s' 'The short update.' \
  | ssh -T user@host 'speak --stdout' \
  | speak --play-stdin
```

iOS SSH playback requires a client forward from local
`127.0.0.1:17820` to remote `127.0.0.1:17820`:

```sh
speak --ios --speed 1.1 <<'SPEAK_TEXT'
The short conversational update goes here. Open the printed link to listen.
SPEAK_TEXT
```

Over SSH, `--play` targets the remote host's speakers. It does not send audio to
the SSH client's device.

Never interpolate untrusted text into a shell command. Never speak secrets,
credentials, personal data, raw logs, code, URLs, or dense lists.

## Spoken style

- Keep progress updates to one or two sentences.
- Keep final summaries to two to four sentences.
- Lead with the outcome, then the most important limitation or next step.
- Preserve exact technical evidence in writing rather than speech.
