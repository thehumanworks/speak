//! `speak` — an on-device text-to-speech CLI for AI agents.
//!
//! Thin wrapper over the `speak-core` SDK: parse arguments, read text from an
//! argument or stdin, then synthesize. Long inputs are split into coherent
//! chunks and synthesized one at a time, so playback and stdout streaming start
//! on the first chunk instead of waiting for the whole document. Standard output
//! carries the selected protocol payload: WAV bytes for `--stdout`, or the one-time
//! playback URL for `--ios`; status and diagnostics go to standard error.

use std::io::{IsTerminal, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use speak_core::{Device, Engine, ModelLocator, SynthesisRequest, BUILTIN_VOICES};

mod ios;
mod player;
mod wavinput;
mod wavstream;

use ios::IosPlaybackServer;
use player::StreamingPlayer;

const DEFAULT_IOS_BIND: &str = "127.0.0.1:17820";
const DEFAULT_IOS_TIMEOUT_SECS: u64 = 300;
const MAX_IOS_TIMEOUT_SECS: u64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SshConnection {
    client_ip: IpAddr,
    server_ip: IpAddr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IosRouteKind {
    Local,
    Tunnel,
    PrivateDirect,
    Explicit,
}

#[derive(Debug, PartialEq, Eq)]
struct IosRoute {
    bind_addr: SocketAddr,
    public_base_url: Option<String>,
    ssh_connection: Option<SshConnection>,
    kind: IosRouteKind,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DeviceArg {
    /// GPU (CoreML on macOS) with automatic CPU fallback.
    Auto,
    /// Force CPU-only inference.
    Cpu,
}

impl From<DeviceArg> for Device {
    fn from(d: DeviceArg) -> Self {
        match d {
            DeviceArg::Auto => Device::Auto,
            DeviceArg::Cpu => Device::Cpu,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "speak",
    version,
    about = "On-device text-to-speech for AI agents (Supertonic-3, ONNX, compiled)."
)]
struct Args {
    /// Text to speak. If omitted, the text is read from standard input.
    text: Option<String>,

    /// Voice: a built-in id (M1..M5 male, F1..F5 female) or a path to a
    /// custom voice style JSON.
    #[arg(short = 'v', long, default_value = "M1")]
    voice: String,

    /// Write a WAV file to this path. If omitted, audio is played aloud, or
    /// streamed to stdout when stdout is not a terminal.
    #[arg(short = 'o', long)]
    out: Option<PathBuf>,

    /// Stream WAV bytes to stdout. Ignored when --out is set.
    #[arg(long)]
    stdout: bool,

    /// Play the audio aloud on this machine. Forces playback even when stdout
    /// is captured. Over SSH this means the remote host's audio device.
    #[arg(long)]
    play: bool,

    /// Read the canonical WAV stream produced by `speak --stdout` from stdin
    /// and play it on this machine. Intended for desktop SSH pipelines.
    #[arg(
        long,
        conflicts_with_all = ["text", "out", "stdout", "play", "ios", "list_voices", "dump_chunks"]
    )]
    play_stdin: bool,

    /// Serve a short-lived browser player for an iPhone or iPad SSH client.
    /// Private SSH networks work directly; public SSH requires a local forward.
    #[arg(long, conflicts_with_all = ["out", "stdout", "play"])]
    ios: bool,

    /// Override the address selected by --ios. Without this option, private SSH
    /// networks use the observed host address and other sessions use loopback.
    #[arg(long, value_name = "ADDR", requires = "ios")]
    ios_bind: Option<SocketAddr>,

    /// Public origin printed by --ios, for a different forwarded local port or
    /// an HTTPS reverse proxy. Example: http://127.0.0.1:8080. No path allowed.
    #[arg(long, value_name = "URL", requires = "ios")]
    ios_url: Option<String>,

    /// Seconds the one-time --ios link remains available. Defaults to 300;
    /// maximum 86400.
    #[arg(long, value_name = "SECONDS", requires = "ios")]
    ios_timeout: Option<u64>,

    /// Language code (en, ko, ja, ...). Use "na" for unknown text.
    #[arg(short = 'l', long, default_value = "en")]
    lang: String,

    /// Denoising steps: higher is better quality but slower.
    #[arg(short = 's', long, default_value_t = 8)]
    steps: usize,

    /// Speech speed factor (0.9-1.5 recommended).
    #[arg(long, default_value_t = 1.05)]
    speed: f32,

    /// Pause inserted between paragraphs (seconds); inter-sentence and
    /// inter-clause pauses are scaled down from this.
    #[arg(long, default_value_t = 0.3)]
    gap: f32,

    /// Inference device: cpu (default) or auto (try GPU/CoreML, fall back to
    /// CPU). CoreML currently can't run this model, so cpu is faster.
    #[arg(long, value_enum, default_value_t = DeviceArg::Cpu)]
    device: DeviceArg,

    /// Model base directory containing onnx/ and voice_styles/. Defaults to
    /// $SUPERTONIC_CACHE_DIR or ~/.cache/supertonic3.
    #[arg(long)]
    model_dir: Option<PathBuf>,

    /// Do not download missing model files; fail if the cache is incomplete.
    /// By default `speak` fetches them from Hugging Face on first run.
    #[arg(long)]
    no_download: bool,

    /// List the built-in voices and exit.
    #[arg(long)]
    list_voices: bool,

    /// Print how the text would be split into streaming chunks (text, length,
    /// and trailing gap) and exit, without loading the model or synthesizing.
    #[arg(long)]
    dump_chunks: bool,

    /// Print extra diagnostics to stderr, such as the inference backend and
    /// time-to-first-audio.
    #[arg(long)]
    verbose: bool,
}

#[derive(Debug, PartialEq)]
enum Sink {
    File(PathBuf),
    Stdout,
    Play,
    Ios,
}

/// Decide where the synthesized audio goes, from the explicit flags, terminal
/// state, and whether this is an interactive SSH session.
///
/// `--out` always writes a file. Otherwise `--ios`, `--stdout`, and `--play`
/// choose their corresponding sinks. With no destination flag, a non-terminal
/// stdout streams bytes and a local terminal plays aloud. An interactive SSH
/// terminal is rejected rather than accidentally opening the remote host's
/// audio device; the error points to `--ios` or explicit `--play`.
fn choose_sink(
    out: Option<PathBuf>,
    ios_flag: bool,
    stdout_flag: bool,
    play_flag: bool,
    stdout_is_terminal: bool,
    interactive_ssh: bool,
) -> Result<Sink> {
    if let Some(path) = out {
        return Ok(Sink::File(path));
    }
    if ios_flag {
        return Ok(Sink::Ios);
    }
    if stdout_flag {
        return Ok(Sink::Stdout);
    }
    if play_flag {
        return Ok(Sink::Play);
    }
    if !stdout_is_terminal {
        return Ok(Sink::Stdout);
    }
    if interactive_ssh {
        bail!(
            "interactive SSH session detected: SSH cannot route the remote audio device to this client. For iPhone/iPad, run with --ios; private SSH networks work directly and public SSH uses a local forward to {DEFAULT_IOS_BIND}. Pass --play only to use the remote host's speakers"
        );
    }
    Ok(Sink::Play)
}

/// Parse the standard `SSH_CONNECTION` value:
/// `<client-ip> <client-port> <server-ip> <server-port>`.
fn parse_ssh_connection(value: &str) -> Option<SshConnection> {
    let mut fields = value.split_whitespace();
    let client_ip = fields.next()?.parse().ok()?;
    fields.next()?.parse::<u16>().ok()?;
    let server_ip = fields.next()?.parse().ok()?;
    fields.next()?.parse::<u16>().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some(SshConnection {
        client_ip,
        server_ip,
    })
}

/// Private/LAN and tailnet addresses can safely support a direct, tokenized
/// playback URL. Public addresses stay on loopback so `--ios` never exposes
/// plaintext audio to the internet by default.
fn is_private_route_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_private() || is_shared_address_space(address),
        IpAddr::V6(address) => (address.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// RFC 6598 shared address space. Tailscale uses this range for IPv4 tailnet
/// addresses, but it is not included in `Ipv4Addr::is_private`.
fn is_shared_address_space(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

/// Select a no-configuration direct route only when both observed SSH endpoints
/// are private. Requiring both sides avoids advertising a server-private address
/// to a public client connected through NAT or a proxy.
fn choose_ios_route(
    explicit_bind: Option<SocketAddr>,
    explicit_url: Option<String>,
    ssh_connection: Option<SshConnection>,
) -> IosRoute {
    let has_explicit_networking = explicit_bind.is_some() || explicit_url.is_some();
    if has_explicit_networking {
        return IosRoute {
            bind_addr: explicit_bind.unwrap_or_else(|| {
                DEFAULT_IOS_BIND
                    .parse()
                    .expect("the built-in iOS bind address must be valid")
            }),
            public_base_url: explicit_url,
            ssh_connection,
            kind: IosRouteKind::Explicit,
        };
    }

    if let Some(connection) = ssh_connection {
        if is_private_route_address(connection.client_ip)
            && is_private_route_address(connection.server_ip)
        {
            return IosRoute {
                // The URL contains the actual ephemeral port selected by the OS.
                // This avoids both port conflicts and one-time client setup.
                bind_addr: SocketAddr::new(connection.server_ip, 0),
                public_base_url: None,
                ssh_connection: Some(connection),
                kind: IosRouteKind::PrivateDirect,
            };
        }
    }

    IosRoute {
        bind_addr: DEFAULT_IOS_BIND
            .parse()
            .expect("the built-in iOS bind address must be valid"),
        public_base_url: None,
        ssh_connection,
        kind: if ssh_connection.is_some() {
            IosRouteKind::Tunnel
        } else {
            IosRouteKind::Local
        },
    }
}

fn sanitize_client_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, ' ' | '.' | '_' | '+' | '-' | '(' | ')' | '/')
        })
    {
        return None;
    }
    Some(value.to_string())
}

/// SSH has no standard client-application identity field. Some clients forward
/// a specific terminal-program hint, and a few use a distinctive `TERM` value.
fn detect_ssh_client_name(
    term_program: Option<&str>,
    lc_terminal: Option<&str>,
    term: Option<&str>,
) -> Option<String> {
    if let Some(name) = term_program.and_then(sanitize_client_name) {
        return Some(name);
    }
    if let Some(name) = lc_terminal.and_then(sanitize_client_name) {
        return Some(name);
    }

    let term = term?.to_ascii_lowercase();
    [
        ("blink", "Blink"),
        ("termius", "Termius"),
        ("xterm-kitty", "kitty"),
        ("wezterm", "WezTerm"),
    ]
    .into_iter()
    .find_map(|(needle, name)| term.contains(needle).then(|| name.to_string()))
}

fn ssh_client_name_from_environment() -> Option<String> {
    let term_program = std::env::var("TERM_PROGRAM").ok();
    let lc_terminal = std::env::var("LC_TERMINAL").ok();
    let term = std::env::var("TERM").ok();
    detect_ssh_client_name(
        term_program.as_deref(),
        lc_terminal.as_deref(),
        term.as_deref(),
    )
}

/// Load the engine, auto-downloading missing model files unless the user opted
/// out or the binary was built without the `download` feature.
fn load_engine(locator: ModelLocator, device: Device, no_download: bool) -> Result<Engine> {
    #[cfg(feature = "download")]
    {
        if no_download {
            Engine::load_with(locator, device)
        } else {
            Engine::load_or_download(locator, device)
        }
    }
    #[cfg(not(feature = "download"))]
    {
        let _ = no_download;
        Engine::load_with(locator, device)
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.play_stdin {
        return wavinput::play_stdin(args.verbose);
    }

    if args.list_voices {
        eprintln!("Built-in voices (M* = male, F* = female):");
        for v in BUILTIN_VOICES {
            println!("{v}");
        }
        eprintln!("\nOr pass a path to a custom voice style JSON via --voice.");
        return Ok(());
    }

    let text = read_text(args.text.clone())?;

    if args.dump_chunks {
        let chunks = speak_core::plan_chunks(&text, args.gap);
        eprintln!("{} chunk(s):", chunks.len());
        for (i, c) in chunks.iter().enumerate() {
            println!(
                "[{:>3}] {:>3} chars, gap {:.2}s | {}",
                i + 1,
                c.text.chars().count(),
                c.gap_after,
                c.text
            );
        }
        return Ok(());
    }

    // Bail before loading the model if there is nothing speakable: some inputs
    // are non-empty yet normalize to no chunks (a lone code block, a horizontal
    // rule, a bare image link), which would otherwise emit silent output and a
    // misleading "Saved 0.00s" message.
    if speak_core::plan_chunks(&text, args.gap).is_empty() {
        bail!(
            "no speakable text after normalization (input was only markup, code, or punctuation)"
        );
    }

    // Decide where the audio goes before doing expensive work, so failures are
    // reported early.
    let sink = choose_sink(
        args.out.clone(),
        args.ios,
        args.stdout,
        args.play,
        std::io::stdout().is_terminal(),
        std::env::var_os("SSH_TTY").is_some(),
    )?;

    let ssh_connection = std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|value| parse_ssh_connection(&value));
    let mut ios_route = (sink == Sink::Ios)
        .then(|| choose_ios_route(args.ios_bind, args.ios_url.clone(), ssh_connection));
    let ios_server = if let Some(route) = &mut ios_route {
        let timeout_secs = args.ios_timeout.unwrap_or(DEFAULT_IOS_TIMEOUT_SECS);
        if timeout_secs == 0 || timeout_secs > MAX_IOS_TIMEOUT_SECS {
            bail!("--ios-timeout must be between 1 and {MAX_IOS_TIMEOUT_SECS} seconds");
        }
        let timeout = Duration::from_secs(timeout_secs);
        let server =
            IosPlaybackServer::bind(route.bind_addr, route.public_base_url.as_deref(), timeout);
        Some(match server {
            Ok(server) => server,
            Err(error) if route.kind == IosRouteKind::PrivateDirect => {
                eprintln!(
                    "Could not bind the detected private SSH host address: {error:#}. Falling back to the loopback tunnel."
                );
                route.bind_addr = DEFAULT_IOS_BIND
                    .parse()
                    .expect("the built-in iOS bind address must be valid");
                route.kind = IosRouteKind::Tunnel;
                IosPlaybackServer::bind(route.bind_addr, None, timeout)
                    .context("could not bind the fallback iOS loopback tunnel")?
            }
            Err(error) => return Err(error),
        })
    } else {
        None
    };

    let locator = match &args.model_dir {
        Some(dir) => ModelLocator::new(dir.clone()),
        None => ModelLocator::from_cache(),
    };

    let mut engine = load_engine(locator, args.device.into(), args.no_download)?;
    if args.verbose {
        eprintln!("Inference backend: {}", engine.backend());
    }
    let request = SynthesisRequest::new(text)
        .lang(args.lang.clone())
        .steps(args.steps)
        .speed(args.speed)
        .silence(args.gap);

    match sink {
        Sink::Play => stream_to_player(&mut engine, &args.voice, &request, args.verbose)?,
        Sink::Stdout => {
            // With --play, also play aloud while streaming bytes to stdout.
            let player = if args.play {
                Some(
                    StreamingPlayer::new(engine.sample_rate())
                        .context("could not open an audio output device for --play")?,
                )
            } else {
                None
            };
            wavstream::stream_to_stdout(
                &mut engine,
                &args.voice,
                &request,
                player.as_ref(),
                args.verbose,
            )?;
            if let Some(player) = &player {
                player.finish_and_wait();
            }
        }
        Sink::File(path) => {
            if args.play {
                synth_to_file_and_play(&mut engine, &args.voice, &request, &path, args.verbose)?;
            } else {
                let audio = engine.speak(&args.voice, &request)?;
                audio.write_wav(&path)?;
                eprintln!(
                    "Saved {:.2}s of audio to {}",
                    audio.duration_secs(),
                    path.display()
                );
            }
        }
        Sink::Ios => {
            let server = ios_server.expect("the iOS sink must have a bound server");
            let route = ios_route
                .as_ref()
                .expect("the iOS sink must have a selected network route");
            synthesize_for_ios(
                &mut engine,
                &args.voice,
                &request,
                server,
                route,
                ssh_client_name_from_environment().as_deref(),
                args.verbose,
            )?;
        }
    }

    clean_exit(engine)
}

/// Synthesize a complete, seekable WAV and expose it through a tokenized,
/// short-lived HTTP player. iOS media playback commonly uses byte ranges, so a
/// fixed-length WAV is preferable to the unknown-length stdout stream here.
fn synthesize_for_ios(
    engine: &mut Engine,
    voice: &str,
    request: &SynthesisRequest,
    server: IosPlaybackServer,
    route: &IosRoute,
    ssh_client_name: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let started = Instant::now();
    let audio = engine.speak(voice, request)?;
    let wav = audio
        .to_wav_bytes()
        .context("failed to encode WAV for iOS playback")?;

    if verbose {
        eprintln!(
            "Prepared {:.2}s of audio in {:.2}s; serving {} bytes from {}.",
            audio.duration_secs(),
            started.elapsed().as_secs_f64(),
            wav.len(),
            server.bind_addr()
        );
    }

    // The URL is the sole stdout protocol output in --ios mode. Explanatory
    // text remains on stderr, and terminals generally make the raw URL tappable.
    println!("{}", server.url());
    std::io::stdout().flush().ok();
    report_ios_route(route, server.bind_addr(), ssh_client_name);
    eprintln!("If autoplay is blocked, tap Play.");
    if !server.bind_addr().ip().is_loopback() && server.url().starts_with("http://") {
        eprintln!(
            "Warning: the playback URL uses unencrypted HTTP on a non-loopback address. Use only on a trusted private network, or put the endpoint behind HTTPS."
        );
    }

    server.serve(&wav)
}

fn report_ios_route(route: &IosRoute, actual_bind: SocketAddr, ssh_client_name: Option<&str>) {
    if let Some(connection) = route.ssh_connection {
        match ssh_client_name {
            Some(name) => {
                eprintln!("Detected SSH client {name} at {}.", connection.client_ip);
            }
            None => {
                eprintln!(
                    "Detected SSH client address {}; standard SSH did not expose the client application name.",
                    connection.client_ip
                );
            }
        }
    }

    match route.kind {
        IosRouteKind::PrivateDirect => {
            eprintln!(
                "Audio ready on private SSH host address {actual_bind}; open the URL above directly. No port forward is needed."
            );
        }
        IosRouteKind::Tunnel => {
            eprintln!(
                "Audio ready on {actual_bind}. Open the URL above through a client-side local forward to remote {actual_bind}. Standard SSH does not let a remote command create that forward."
            );
        }
        IosRouteKind::Explicit => {
            eprintln!("Audio ready using the requested iOS network settings; open the URL above.");
        }
        IosRouteKind::Local => {
            eprintln!("Audio ready on {actual_bind}; open the URL above on this device.");
        }
    }
}

/// Synthesize chunk by chunk and play each chunk as soon as it is ready, so the
/// first words are heard within a second or so rather than after the whole
/// document is synthesized.
fn stream_to_player(
    engine: &mut Engine,
    voice: &str,
    request: &SynthesisRequest,
    verbose: bool,
) -> Result<()> {
    let sample_rate = engine.sample_rate();
    let player = StreamingPlayer::new(sample_rate)
        .context("could not open an audio output device; use --out FILE or --stdout instead")?;

    let show_progress = std::io::stderr().is_terminal();
    let start = Instant::now();
    let mut first_audio: Option<f64> = None;
    let mut total_audio = 0.0f64;

    engine.speak_stream(voice, request, |chunk| {
        if first_audio.is_none() {
            let ttfa = start.elapsed().as_secs_f64();
            first_audio = Some(ttfa);
            if verbose {
                eprintln!("First audio ready after {ttfa:.2}s; playback starts now.");
            }
        }
        let gap = (chunk.gap_after * sample_rate as f32) as usize;
        player.push(&chunk.audio.samples);
        player.push_silence(gap);
        total_audio += chunk.audio.duration_secs() as f64 + chunk.gap_after as f64;

        if show_progress {
            eprint!("\rSpeaking… chunk {}/{}", chunk.index + 1, chunk.total);
            let _ = std::io::stderr().flush();
        }
        // Keep the lookahead buffer bounded: synthesis runs several times
        // faster than playback, so without this the whole document would be
        // synthesized into memory immediately.
        player.wait_until_buffer_below(sample_rate as usize * 20);
        if player.is_stopped() {
            bail!("audio output device stopped during playback");
        }
        Ok(())
    })?;

    if show_progress {
        eprint!("\r\x1b[K");
        let _ = std::io::stderr().flush();
    }
    player.finish_and_wait();

    if verbose {
        if let Some(ttfa) = first_audio {
            eprintln!(
                "Streamed {:.1}s of audio; first audio after {:.2}s.",
                total_audio, ttfa
            );
        }
    }
    Ok(())
}

/// Synthesize to a WAV file while also playing it back as it is generated, in a
/// single synthesis pass.
fn synth_to_file_and_play(
    engine: &mut Engine,
    voice: &str,
    request: &SynthesisRequest,
    path: &std::path::Path,
    verbose: bool,
) -> Result<()> {
    let sample_rate = engine.sample_rate();
    let player = StreamingPlayer::new(sample_rate)
        .context("could not open an audio output device; use --out FILE alone to just save")?;
    let show_progress = std::io::stderr().is_terminal();
    let mut samples: Vec<f32> = Vec::new();

    engine.speak_stream(voice, request, |chunk| {
        let gap = (chunk.gap_after * sample_rate as f32) as usize;
        player.push(&chunk.audio.samples);
        player.push_silence(gap);
        samples.extend_from_slice(&chunk.audio.samples);
        samples.extend(std::iter::repeat_n(0.0, gap));
        if show_progress {
            eprint!("\rSpeaking… chunk {}/{}", chunk.index + 1, chunk.total);
            let _ = std::io::stderr().flush();
        }
        player.wait_until_buffer_below(sample_rate as usize * 20);
        if player.is_stopped() {
            bail!("audio output device stopped during playback");
        }
        Ok(())
    })?;

    if show_progress {
        eprint!("\r\x1b[K");
        let _ = std::io::stderr().flush();
    }

    let audio = speak_core::Audio {
        samples,
        sample_rate,
    };
    audio.write_wav(path)?;
    eprintln!(
        "Saved {:.2}s of audio to {}",
        audio.duration_secs(),
        path.display()
    );
    let _ = verbose;
    player.finish_and_wait();
    Ok(())
}

/// Read text from the positional argument, falling back to stdin.
fn read_text(arg: Option<String>) -> Result<String> {
    let raw = match arg {
        Some(t) => t,
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("failed to read text from stdin")?;
            buf
        }
    };
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        bail!("no text to speak: pass text as an argument or pipe it via stdin");
    }
    Ok(trimmed)
}

/// Exit without running destructors. On macOS, dropping ONNX Runtime sessions
/// during normal shutdown can hit a mutex-cleanup crash, so the upstream
/// example bypasses cleanup with `_exit`. We mirror that.
fn clean_exit(engine: Engine) -> Result<()> {
    std::io::stdout().flush().ok();
    std::io::stderr().flush().ok();
    #[cfg(unix)]
    unsafe {
        std::mem::forget(engine);
        libc::_exit(0);
    }
    #[cfg(not(unix))]
    {
        drop(engine);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav() -> Option<PathBuf> {
        Some(PathBuf::from("out.wav"))
    }

    #[test]
    fn out_path_always_writes_a_file() {
        // --out wins over every other flag in the routing function. Clap rejects
        // --out with --ios before this point in real invocations.
        assert_eq!(
            choose_sink(wav(), true, true, true, false, true).unwrap(),
            Sink::File("out.wav".into())
        );
    }

    #[test]
    fn explicit_ios_uses_browser_playback() {
        assert_eq!(
            choose_sink(None, true, false, false, true, true).unwrap(),
            Sink::Ios
        );
    }

    #[test]
    fn explicit_stdout_streams_even_on_a_terminal() {
        assert_eq!(
            choose_sink(None, false, true, false, true, true).unwrap(),
            Sink::Stdout
        );
    }

    #[test]
    fn play_forces_remote_playback_in_ssh() {
        assert_eq!(
            choose_sink(None, false, false, true, true, true).unwrap(),
            Sink::Play
        );
    }

    #[test]
    fn pipe_without_play_streams_to_stdout() {
        assert_eq!(
            choose_sink(None, false, false, false, false, true).unwrap(),
            Sink::Stdout
        );
    }

    #[test]
    fn local_terminal_without_flags_plays_aloud() {
        assert_eq!(
            choose_sink(None, false, false, false, true, false).unwrap(),
            Sink::Play
        );
    }

    #[test]
    fn interactive_ssh_without_destination_is_actionable_error() {
        let error = choose_sink(None, false, false, false, true, true).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("interactive SSH session detected"));
        assert!(message.contains("--ios"));
        assert!(message.contains("--play"));
    }

    #[test]
    fn parses_standard_ssh_connection_metadata() {
        assert_eq!(
            parse_ssh_connection("100.101.102.103 54321 100.91.92.93 22"),
            Some(SshConnection {
                client_ip: "100.101.102.103".parse().unwrap(),
                server_ip: "100.91.92.93".parse().unwrap(),
            })
        );
        assert_eq!(
            parse_ssh_connection("fd7a:115c:a1e0::1 54321 fd7a:115c:a1e0::2 22"),
            Some(SshConnection {
                client_ip: "fd7a:115c:a1e0::1".parse().unwrap(),
                server_ip: "fd7a:115c:a1e0::2".parse().unwrap(),
            })
        );
        assert_eq!(parse_ssh_connection("not a connection"), None);
        assert_eq!(
            parse_ssh_connection("100.64.0.1 1234 100.64.0.2 22 extra"),
            None
        );
    }

    #[test]
    fn ios_uses_private_ssh_address_without_a_tunnel() {
        let connection = SshConnection {
            client_ip: "100.101.102.103".parse().unwrap(),
            server_ip: "100.91.92.93".parse().unwrap(),
        };
        let route = choose_ios_route(None, None, Some(connection));

        assert_eq!(route.bind_addr, "100.91.92.93:0".parse().unwrap());
        assert_eq!(route.kind, IosRouteKind::PrivateDirect);
        assert_eq!(route.ssh_connection, Some(connection));
    }

    #[test]
    fn private_route_detection_covers_lan_tailnet_and_ipv6_ula() {
        for address in [
            "10.0.0.8",
            "172.16.2.3",
            "192.168.1.20",
            "100.64.0.1",
            "100.127.255.254",
            "fd7a:115c:a1e0::1",
        ] {
            assert!(
                is_private_route_address(address.parse().unwrap()),
                "{address} should support private direct playback"
            );
        }
        for address in ["127.0.0.1", "100.128.0.1", "203.0.113.20", "::1"] {
            assert!(
                !is_private_route_address(address.parse().unwrap()),
                "{address} should not select private direct playback"
            );
        }
    }

    #[test]
    fn ios_keeps_public_ssh_sessions_on_the_loopback_tunnel() {
        let connection = SshConnection {
            client_ip: "198.51.100.40".parse().unwrap(),
            server_ip: "203.0.113.20".parse().unwrap(),
        };
        let route = choose_ios_route(None, None, Some(connection));

        assert_eq!(route.bind_addr, DEFAULT_IOS_BIND.parse().unwrap());
        assert_eq!(route.kind, IosRouteKind::Tunnel);
    }

    #[test]
    fn explicit_ios_network_options_disable_auto_routing() {
        let connection = SshConnection {
            client_ip: "100.101.102.103".parse().unwrap(),
            server_ip: "100.91.92.93".parse().unwrap(),
        };
        let bind_addr = "127.0.0.1:8080".parse().unwrap();
        let route = choose_ios_route(
            Some(bind_addr),
            Some("https://audio.example.test".to_string()),
            Some(connection),
        );

        assert_eq!(route.bind_addr, bind_addr);
        assert_eq!(
            route.public_base_url.as_deref(),
            Some("https://audio.example.test")
        );
        assert_eq!(route.kind, IosRouteKind::Explicit);
    }

    #[test]
    fn identifies_known_ssh_clients_only_from_specific_terminal_hints() {
        assert_eq!(
            detect_ssh_client_name(Some("Blink"), None, Some("xterm-256color")).as_deref(),
            Some("Blink")
        );
        assert_eq!(
            detect_ssh_client_name(None, None, Some("xterm-kitty")).as_deref(),
            Some("kitty")
        );
        assert_eq!(
            detect_ssh_client_name(None, None, Some("xterm-256color")),
            None
        );
        assert_eq!(
            detect_ssh_client_name(Some("\u{1b}]0;spoofed"), None, None),
            None
        );
    }
}
