---
name: speak
description: Use the local `speak` text-to-speech CLI for brief, conversational audible progress updates and a spoken final summary while preserving important details in the written chat. Use when the user invokes `$speak`, asks to hear responses aloud, requests voice feedback while Codex works, or wants a hands-free conversational task experience.
---

# Speak

Use audible speech as a companion to the written response, never as its
replacement.

## Workflow

1. Verify the command with `command -v speak`. Read `speak --help` only when its
   behavior is uncertain.
2. Speak only useful milestones: the start of substantial work, a meaningful
   result or change of direction, a blocker that needs attention, and the final
   outcome. Do not narrate every tool call.
3. Write the normal commentary update first, then play a shorter conversational
   version aloud.
4. Invoke `speak` with `--play` because captured command output may otherwise
   become WAV bytes instead of audible playback. Prefer a natural speed around
   `1.1`.
5. Before sending the final written answer, play its concise conversational
   summary aloud. Keep the complete evidence and reference material in the
   written answer.
6. If speech fails, report that briefly in writing and continue the task. Do not
   let optional audio block delivery of the written result.

Use standard input when the text contains shell-sensitive characters:

```sh
speak --play --speed 1.1 <<'SPEAK_TEXT'
The short conversational update goes here.
SPEAK_TEXT
```

Never interpolate untrusted text into a shell command.

## Spoken Style

- Use relaxed, direct prose with contractions where natural.
- Keep progress updates to one or two sentences.
- Keep the final spoken summary to two to four sentences unless the user asks
  for more detail.
- Lead with the outcome, then mention the most important next step, limitation,
  or decision.
- Translate technical detail into plain language. Leave commands, code, long
  paths, URLs, citations, raw logs, and dense lists in writing.
- Never speak secrets, credentials, personal data, or other sensitive content.
- When a user decision is required, speak the short question and also write the
  exact question in chat.

## Written Record

- Continue to provide normal written progress updates and a self-contained final
  answer.
- Record exact changes, files, validation results, limitations, and reusable
  instructions in writing when relevant.
- Keep spoken and written claims consistent. Treat the spoken response as a
  summary, not as evidence that work succeeded.
