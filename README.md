# speak

A compiled, on-device text-to-speech stack built on
[Supertonic-3](https://github.com/supertone-inc/supertonic) and ONNX Runtime.
It produces a self-contained `speak` CLI for AI agents and a reusable
`speak-core` Rust crate for native applications and services.

## Architecture

The TTS pipeline is four ONNX models, a codepoint-based text front-end, and
per-voice style files. The executable contains ONNX Runtime; the roughly 385 MB
of model weights are loaded from a per-user cache rather than embedded in the
binary.

```text
speak/
  Cargo.toml
  src/
    main.rs       # CLI, destination routing, SSH/iOS hand-off
    ios.rs        # short-lived browser playback endpoint
    player.rs     # local cpal playback
    wavstream.rs  # incremental WAV output
    wavinput.rs   # incremental WAV input/local playback
  speak-core/
    src/lib.rs    # Engine, ModelLocator, SynthesisRequest, Audio
    src/helper.rs # vendored upstream inference pipeline
```

The CLI is a thin layer over `speak-core`. New front-ends should depend on the
SDK rather than reaching into the model plumbing.

## Install

### Nix package (Linux)

With Nix's `nix-command` and `flakes` features enabled, from this checkout:

```bash
nix run . -- --list-voices
nix run . -- --play "Hello."
nix profile install .#speak
```

The flake provides `packages.<system>.speak` and the default app for
`x86_64-linux` and `aarch64-linux`. ONNX Runtime **1.24.4** is a separate,
automatically installed dependency, pinned by the immutable nixpkgs revision in
`flake.nix`. It is dynamically linked, not embedded in the executable. The
`ort-sys` build is explicitly prevented from downloading another runtime, and
Cargo builds offline from `Cargo.lock` sources. Nix fetches declared build inputs
before entering the build sandbox. Model weights are separate: first use still
downloads them to the normal per-user cache, or use `--no-download` and an
already populated `--model-dir`.

For NixOS or Home Manager, add this package as a flake input (use the checkout's
absolute path until your chosen revision is published):

```nix
inputs.speak.url = "path:/absolute/path/to/speak";
```

Pass `inputs` through your existing `specialArgs` (NixOS) or `extraSpecialArgs`
(Home Manager). Then use either of these in the corresponding configuration:

```nix
{ inputs, pkgs, ... }: {
  environment.systemPackages = [
    inputs.speak.packages.${pkgs.stdenv.hostPlatform.system}.speak
  ];
}
```

```nix
{ inputs, pkgs, ... }: {
  home.packages = [
    inputs.speak.packages.${pkgs.stdenv.hostPlatform.system}.speak
  ];
}
```

Once published, `github:thehumanworks/speak/<commit-or-branch>` can replace the
path input and `.` in the commands. Keep speak's nixpkgs input pinned rather
than making it follow your system's input: the runtime version must match the
Rust binding. No NixOS/Home Manager module or new settings format is needed.

On ordinary Linux and NixOS, the package uses your existing audio service and
ALSA configuration. On WSL 2 with WSLg, keep `PULSE_SERVER` and use:

```bash
nix run . -- --play "Hello from WSL."
```

The package supplies ALSA plugins but does not select a default device or edit
audio settings; see the WSL guidance below if ALSA has no default device.
See the [Nix flake manual](https://nix.dev/manual/nix/2.28/command-ref/new-cli/nix3-flake.html)
for enabling flakes and selecting package outputs.

### Setup with system dependencies (Linux)

From a checkout, with Node 18+ installed:

```bash
git clone https://github.com/thehumanworks/speak.git
cd speak
npm run setup -- --dry-run
npm run setup
node bin/speak.js --play "Hello."
```

Setup installs runtime and source-build system dependencies **by default**, then
runs the existing binary downloader/source fallback. Install a current Rust
toolchain separately if a source build is needed. It shows every command and
asks before changing packages. Run as your normal user; only package commands
use sudo. The binary is installed into this package's `vendor/` directory.

```bash
# Already have a binary: install runtime dependencies only
node scripts/setup.js --deps-only --runtime-only
# Prepare for Cargo (no binary download/build by setup)
node scripts/setup.js --deps-only
# Automation: no prompts, including sudo (requires root or configured sudo access)
node scripts/setup.js --deps-only --yes
```

Without a terminal, setup requires `--yes`; it uses `sudo -n` and stops if sudo
needs a password. `--dry-run` never executes commands or writes files. Root can
use `--deps-only`, but downloads/builds must run as a normal user. Package
operations can be repeated; a failed command stops setup immediately. Earlier
successful package operations are not rolled back.

The explicit matrix is Debian, Ubuntu, Fedora and Arch Linux (`ID` from
`/etc/os-release`, not guessed from `ID_LIKE`). Other distributions, including
Alpine, openSUSE and derivatives with different IDs, stop with manual dependency
guidance. Nix users should use the package below instead. This is a package-name
matrix, not a guarantee that every release can run the same prebuilt binary:
older glibc systems may need a source build.

| Distribution | Runtime packages (in addition to `ca-certificates`, `tar`) | Extra source-build packages |
|---|---|---|
| Debian / Ubuntu | `libasound2-plugins`, `libstdc++6` | `build-essential`, `pkg-config`, `libasound2-dev`, `libssl-dev` |
| Fedora | `alsa-lib`, `libstdc++`; `alsa-plugins-pulseaudio` on WSLg | `gcc`, `gcc-c++`, `make`, `pkgconf-pkg-config`, `alsa-lib-devel`, `openssl-devel` |
| Arch | `alsa-lib`, `alsa-plugins`, `libpulse`, `gcc-libs` | `base-devel`, `openssl` |

Debian/Ubuntu's plugin package pulls in the correct ALSA runtime, including the
`libasound2` / `libasound2t64` transition. Arch users must keep their system fully
upgraded; setup deliberately does not perform a partial database refresh or a
whole-system upgrade. Package names and plugin dependencies can be inspected in
the [Debian](https://packages.debian.org/trixie/libasound2-plugins),
[Ubuntu](https://packages.ubuntu.com/noble/libasound2-plugins),
[Fedora](https://packages.fedoraproject.org/pkgs/alsa-plugins/alsa-plugins-pulseaudio/)
and [Arch](https://archlinux.org/packages/extra/x86_64/alsa-plugins/) catalogs.

macOS and Windows use their native audio stack; setup installs no system packages
there. Source builds still require Rust plus Xcode Command Line Tools on macOS,
or MSVC Build Tools on Windows.

### npm or Bun

The npm package installs a matching prebuilt binary from GitHub Releases and
falls back to a source build when necessary:

```bash
bunx @nothumanwork/speak --list-voices
npx -y @nothumanwork/speak --list-voices
```

The automatic npm lifecycle never installs system packages or invokes sudo.
For dependency installation, use `npm run setup` above, or run `speak-setup`
after a global package install. To invoke setup from the published package
without running its lifecycle first:

```bash
npx --yes --ignore-scripts --package=@nothumanwork/speak speak-setup
```

`--yes` here accepts npx's package download; setup still shows and confirms its
own plan. Pass a trailing `--yes` to accept setup's plan noninteractively.

You can also install directly from this GitHub repository:

```bash
npx -y github:thehumanworks/speak --list-voices
```

Useful installer overrides:

- `SPEAK_VERSION=v0.1.0` pins a release tag.
- `SPEAK_REPO=owner/repo` selects another release repository.
- `SPEAK_GITHUB_TOKEN` or `GITHUB_TOKEN` authenticates release downloads.
- `SPEAK_NPM_SKIP_DOWNLOAD=1` skips binary installation for packaging tests.

### Cargo

```bash
node scripts/setup.js --deps-only  # Linux dependencies; Node 18+ required
cargo install --path .
```

For a CPU-only, offline build with no automatic model download:

```bash
cargo install --path . --no-default-features
```

To keep automatic downloads while disabling CoreML:

```bash
cargo install --path . --no-default-features --features download
```

### Build from a checkout

```bash
cargo build --release
# target/release/speak
```

The first build downloads and links ONNX Runtime. The first synthesis downloads
the model cache unless downloads were disabled.

## Model cache

By default, model assets are stored under `~/.cache/supertonic3`:

```text
~/.cache/supertonic3/
  onnx/{duration_predictor,text_encoder,vector_estimator,vocoder}.onnx
  onnx/{tts.json,unicode_indexer.json}
  voice_styles/{M1..M5,F1..F5}.json
```

Use `--model-dir` or `SUPERTONIC_CACHE_DIR` to choose another base directory.
Use `--no-download` to fail instead of fetching missing files. Downloads are
pinned to an immutable Hugging Face revision, written atomically, and protected
by a lock so concurrent first runs do not download the same files twice.

## CLI examples

```bash
# Play through this machine's default audio device
speak "Hello from a compiled binary."

# Explicit playback, including when stdout is captured by an agent
speak "Build complete." --play

# Save a seekable WAV
speak "Save me to disk." --voice F1 --out hello.wav

# Read text from stdin
echo "Piped in from stdin." | speak --out out.wav

# Stream WAV bytes incrementally
speak "Pipe me." --stdout | ffplay -nodisp -autoexit -loglevel error -i pipe:0

# Inspect long-text segmentation without loading the model
speak "$(cat long-document.md)" --dump-chunks

# List voices
speak --list-voices
```

Common flags:

- `-v, --voice`: `M1` through `M5`, `F1` through `F5`, or a custom style JSON.
- `-o, --out`: write a WAV file.
- `--stdout`: stream WAV bytes to stdout.
- `--play`: play on the machine running `speak`.
- `--play-stdin`: play the canonical WAV received on stdin without loading the model.
- `--ios`: expose a short-lived browser player for an iPhone or iPad SSH client.
- `-l, --lang`, `-s, --steps`, `--speed`, and `--gap`: synthesis controls.
- `--device`, `--model-dir`, `--no-download`, `--verbose`.

Output is mode-specific: `--stdout` writes only WAV bytes to stdout, while
`--ios` writes only the tappable playback URL to stdout. Progress, diagnostics,
paths, and warnings are written to stderr.

## Listen over SSH from a desktop client

Install `speak` on both machines. Synthesize on the remote host through a
non-PTY SSH channel and play the returned WAV stream on the local machine:

```bash
printf '%s' 'Synthesized remotely and played locally.' \
  | ssh -T user@host 'speak --stdout' \
  | speak --play-stdin
```

`--play-stdin` validates the canonical mono PCM16 WAV emitted by `--stdout` and
plays it incrementally. It does not load the TTS model. `ssh -T` matters: do not
send binary WAV data through an interactive PTY.

An external local player also works:

```bash
printf '%s' 'Remote audio.' \
  | ssh -T user@host 'speak --stdout' \
  | ffplay -nodisp -autoexit -loglevel error -i pipe:0
```

## Listen from an iPhone or iPad SSH client

SSH terminal channels do not carry an audio-device abstraction. Running
`--play` in an SSH shell therefore targets the **remote host's** sound device,
not the iPhone or iPad. `--ios` creates a short-lived browser player and chooses
the safest reachable route available.

Run in the remote shell:

```bash
speak --ios "This audio was synthesized remotely and is playing on iOS."
```

`speak` inspects the standard `SSH_CONNECTION` addresses:

- When both the client and host use private LAN, VPN, RFC 6598 (including
  Tailscale), or IPv6 ULA addresses, it binds the exact private host address on
  an available port. Tap the printed URL directly; no forwarding setup is
  needed.
- For a public, NATed, or proxied SSH connection, it stays on remote loopback
  and prints a `http://127.0.0.1:17820/...` URL. Configure the client-side local
  forward below, then run `speak --ios` again.

The command reports the SSH client IP address. It also reports a client
application name when the client supplies a specific `TERM_PROGRAM`,
`LC_TERMINAL`, or distinctive `TERM` value. Standard SSH does not expose a
reliable client-application identity, so generic clients remain unnamed.

### Public or proxied SSH: configure a local forward

Forward this address in the iPhone or iPad SSH client:

```text
local  127.0.0.1:17820
remote 127.0.0.1:17820
```

In an OpenSSH-compatible client, the equivalent connection is:

```bash
ssh -L 17820:127.0.0.1:17820 user@host
```

Keep the SSH connection and forward active while listening.

The remote command cannot add this forward automatically. Local forwarding is
owned by the SSH client/transport, and the remote shell only has its session
channel. Once configured, the forward can remain part of the saved SSH host.

For either route, `speak`:

1. synthesizes a complete, seekable WAV;
2. creates a random 128-bit bearer path;
3. prints only the one-time URL to stdout;
4. serves an iOS-compatible player with byte-range support; and
5. shuts down after the audio is fetched/played or after five minutes.

Tap the printed URL. Safari may require one tap on the Play control because iOS
can block audible autoplay.

### Alternative local port

When the iOS client forwards a different local port, keep the remote destination
at `17820` and override only the URL that `speak` prints:

```text
local  127.0.0.1:8080
remote 127.0.0.1:17820
```

```bash
speak --ios --ios-url http://127.0.0.1:8080 "Hello."
```

### Explicit private-network route

Private SSH connections are detected automatically. When metadata is unavailable
or an HTTPS proxy supplies the public URL, override the route explicitly:

```bash
speak --ios \
  --ios-bind 0.0.0.0:17820 \
  --ios-url http://100.64.0.10:17820 \
  "Hello over the private network."
```

Do not expose the plain-HTTP endpoint directly to the public internet. Use an
SSH tunnel, a trusted private network, or an authenticated HTTPS reverse proxy.
The random URL is a bearer credential until the player exits.

### iOS options

| Option | Default | Purpose |
|---|---:|---|
| `--ios` | off | Select browser playback and automatically choose private-direct or loopback-tunnel routing. |
| `--ios-bind <ADDR>` | automatic | Override the remote address on which the temporary server listens. |
| `--ios-url <ORIGIN>` | bind origin | Origin printed to the user; useful when the forwarded local port differs. |
| `--ios-timeout <SECONDS>` | `300` | Link lifetime, from 1 to 86,400 seconds. |

`--ios` conflicts with `--out`, `--stdout`, and `--play`. `--play-stdin` is a
separate no-synthesis mode for desktop clients. In an interactive SSH
terminal, a bare `speak "text"` now fails with guidance instead of silently
attempting remote-device playback. Pass `--ios` for iOS/browser playback or
`--play` explicitly when the remote machine's speakers are intentional.

## Streaming long documents

Long inputs are split into coherent chunks rather than synthesized in one shot.
Sentences are grouped to roughly 240 characters without cutting words, decimal
references, common abbreviations, or list markers. Playback and stdout streaming
start on the first synthesized chunk, while later chunks are generated ahead of
the consumer.

Markdown emphasis, headings, rules, links, and bullet markers are reduced to
spoken text. Ordered-list numbers and sentence structure are preserved. Pause
length depends on whether a chunk ends a clause, sentence, or paragraph; tune the
paragraph pause with `--gap`.

The `--ios` path deliberately uses a completed WAV rather than the unknown-length
stdout stream because mobile media players commonly probe content length and
request byte ranges before or during playback.

## Device and performance

CPU is the default and recommended device. On Apple Silicon, Supertonic-3 already
runs faster than real time. `--device auto` attempts the CoreML execution provider
and falls back to CPU, but the current dynamic-shape graph is not accepted by
CoreML, making the failed attempt slower than selecting CPU directly.

Local playback uses `cpal`: CoreAudio on macOS, ALSA on Linux, and WASAPI on
Windows. Browser playback does not require an audio device on the remote host.

### WSL 2 playback

`--play` can use Windows speakers through WSL 2 + WSLg. WSLg supplies its own
PulseAudio server and `PULSE_SERVER`; the ALSA PulseAudio plugin bridges CPAL to
that server. See [Microsoft's WSLg architecture](https://github.com/microsoft/wslg#wslg-system-distro).
Inside your WSL distribution, run setup above and then:

```bash
printf '%s\n' "$PULSE_SERVER"
node bin/speak.js --play "Hello from WSL."
```

Setup never writes `.asoundrc`, `/etc/asound.conf`, or custom ALSA fragments,
and never starts a PulseAudio daemon. Fedora's WSLg plugin package itself adds
a default ALSA route, which setup reports before confirmation; review custom
`conf.d` routing before accepting that package. Existing top-level user/system
configuration is left intact.

If ALSA still cannot find a default device, inspect your existing configuration
and, only if appropriate, merge these entries into `~/.asoundrc`:

```text
pcm.!default { type pulse }
ctl.!default { type pulse }
```

Do not replace an existing configuration blindly. Keep the WSLg-provided
`PULSE_SERVER`; do not launch another daemon inside WSLg. WSL 1 or WSL without
WSLg needs separate audio setup; writing a WAV with `--out` still works.

### CLI configuration

The CLI does not read a user settings/config file. Use CLI flags for voices,
speed, output destination and other settings. `SUPERTONIC_CACHE_DIR` selects
the model cache (or use `--model-dir`); there is no NixOS/Home Manager settings
module to configure. The model's `tts.json` is model data, not CLI preferences.

## SDK

```rust
use speak_core::{Device, Engine, ModelLocator, SynthesisRequest};

let mut engine = Engine::load_or_download(
    ModelLocator::from_cache(),
    Device::Cpu,
)?;

let audio = engine.speak(
    "F1",
    &SynthesisRequest::new("Hello.").speed(1.0).steps(12),
)?;

audio.write_wav("hello.wav")?;
let wav_bytes = audio.to_wav_bytes()?;
let pcm = &audio.samples;
```

For incremental consumers, `Engine::speak_stream` invokes a callback as soon as
each chunk is synthesized:

```rust
engine.speak_stream("F1", &SynthesisRequest::new(long_text), |chunk| {
    player.enqueue(&chunk.audio.samples);
    Ok(())
})?;
```

`Engine` is mutable because its ONNX sessions are not shared concurrently. A
server should place each engine behind a mutex or maintain a small engine pool.

## Validation

Installer tests use mocked process execution and real temporary files; they do
not install host packages or validate actual audio playback:

```bash
npm run test:setup
npm run test:npm-package
```

CI also exercises actual dependency installation on Ubuntu and in isolated
Debian, Fedora and Arch containers. Container WSLg detection is simulated to
check plugin installation, not playback. This does not establish audible
Linux/WSL/Windows playback support. The installer is
JavaScript and executes argument arrays without a shell; ShellCheck does not
apply to it.

The Nix package has build/install checks for dynamic ONNX linkage, its runtime
store path, and device-free CLI commands:

```bash
nix flake check --print-build-logs
nix build .#speak --print-build-logs
nix path-info --recursive .#speak | grep onnxruntime-1.24.4
```

The Linux CI job runs these checks for x86_64. The aarch64 output is defined but
must be built on an aarch64 runner or configured remote builder before claiming
platform validation. None of these checks validate audible playback.

Run the default and portable configurations:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo test --workspace --no-default-features
```

## Licensing

`speak-core/src/helper.rs` is vendored from the MIT-licensed Supertonic example.
Supertonic-3 model weights use the OpenRAIL-M license; review that license before
redistributing models or generated audio.
