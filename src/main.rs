//! `speak` — an on-device text-to-speech CLI for AI agents.
//!
//! Thin wrapper over the `speak-core` SDK: parse arguments, read text from an
//! argument or stdin, synthesize, then save to a file, stream WAV to stdout, or
//! play it aloud. All status text goes to stderr so stdout can carry raw audio.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use speak_core::{Device, Engine, ModelLocator, SynthesisRequest, BUILTIN_VOICES};

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

    /// Play the audio aloud (macOS: afplay). Forces playback even when stdout
    /// is not a terminal, e.g. when an agent or a pipe captures stdout.
    /// Combine with --out to both save a file and play it.
    #[arg(long)]
    play: bool,

    /// Language code (en, ko, ja, ...). Use "na" for unknown text.
    #[arg(short = 'l', long, default_value = "en")]
    lang: String,

    /// Denoising steps: higher is better quality but slower.
    #[arg(short = 's', long, default_value_t = 8)]
    steps: usize,

    /// Speech speed factor (0.9-1.5 recommended).
    #[arg(long, default_value_t = 1.05)]
    speed: f32,

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
}

#[derive(Debug, PartialEq)]
enum Sink {
    File(PathBuf),
    Stdout,
    Play,
}

/// Decide where the synthesized audio goes, from the flags and whether stdout
/// is a terminal.
///
/// `--out` always writes a file. Otherwise an explicit `--stdout` streams WAV
/// bytes. Otherwise `--play` forces audible playback — this is what lets an
/// agent run `speak --play` with a captured (non-terminal) stdout and still
/// hear it, rather than having the bytes silently dumped into the pipe.
/// With no destination flag, a non-terminal stdout streams bytes (handy for
/// piping) and a real terminal plays aloud.
fn choose_sink(
    out: Option<PathBuf>,
    stdout_flag: bool,
    play_flag: bool,
    stdout_is_terminal: bool,
) -> Sink {
    match (out, stdout_flag, play_flag, stdout_is_terminal) {
        (Some(path), _, _, _) => Sink::File(path),
        (None, true, _, _) => Sink::Stdout,
        (None, false, true, _) => Sink::Play,
        (None, false, false, false) => Sink::Stdout,
        (None, false, false, true) => Sink::Play,
    }
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

    if args.list_voices {
        eprintln!("Built-in voices (M* = male, F* = female):");
        for v in BUILTIN_VOICES {
            println!("{v}");
        }
        eprintln!("\nOr pass a path to a custom voice style JSON via --voice.");
        return Ok(());
    }

    let text = read_text(args.text)?;

    // Decide where the audio goes before doing expensive work, so failures are
    // reported early.
    let sink = choose_sink(
        args.out.clone(),
        args.stdout,
        args.play,
        std::io::stdout().is_terminal(),
    );

    let locator = match &args.model_dir {
        Some(dir) => ModelLocator::new(dir.clone()),
        None => ModelLocator::from_cache(),
    };

    let mut engine = load_engine(locator, args.device.into(), args.no_download)?;
    let request = SynthesisRequest::new(text)
        .lang(args.lang.clone())
        .steps(args.steps)
        .speed(args.speed);
    let audio = engine.speak(&args.voice, &request)?;

    match sink {
        Sink::File(path) => {
            audio.write_wav(&path)?;
            eprintln!(
                "Saved {:.2}s of audio to {}",
                audio.duration_secs(),
                path.display()
            );
            if args.play {
                play(&audio)?;
            }
        }
        Sink::Stdout => {
            let bytes = audio.to_wav_bytes()?;
            let mut out = std::io::stdout().lock();
            out.write_all(&bytes)
                .context("failed to write WAV to stdout")?;
            out.flush().ok();
            if args.play {
                play(&audio)?;
            }
        }
        Sink::Play => play(&audio)?,
    }

    clean_exit(engine)
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

/// Play audio aloud. Wired up for macOS via `afplay`.
fn play(audio: &speak_core::Audio) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("speak-{}.wav", std::process::id()));
        audio.write_wav(&tmp)?;
        let status = std::process::Command::new("afplay")
            .arg(&tmp)
            .status()
            .context("failed to launch afplay")?;
        let _ = std::fs::remove_file(&tmp);
        if !status.success() {
            bail!("afplay exited with {status}");
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = audio;
        bail!("live playback is only wired up for macOS (afplay); use --out FILE or --stdout instead");
    }
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
        // --out wins over every other flag and over the terminal state.
        assert_eq!(choose_sink(wav(), false, false, true), Sink::File("out.wav".into()));
        assert_eq!(choose_sink(wav(), true, true, false), Sink::File("out.wav".into()));
    }

    #[test]
    fn explicit_stdout_streams_even_on_a_terminal() {
        assert_eq!(choose_sink(None, true, false, true), Sink::Stdout);
    }

    #[test]
    fn play_forces_playback_when_stdout_is_captured() {
        // The agent case: stdout is a pipe (not a terminal), but --play means
        // play aloud rather than dump WAV bytes into the pipe. This is the
        // behavior the old `stdout || !is_terminal` heuristic got wrong.
        assert_eq!(choose_sink(None, false, true, false), Sink::Play);
    }

    #[test]
    fn pipe_without_play_streams_to_stdout() {
        assert_eq!(choose_sink(None, false, false, false), Sink::Stdout);
    }

    #[test]
    fn terminal_without_flags_plays_aloud() {
        assert_eq!(choose_sink(None, false, false, true), Sink::Play);
    }

    #[test]
    fn explicit_stdout_takes_precedence_over_play_for_the_primary_sink() {
        // --stdout still streams; --play is applied additively in main().
        assert_eq!(choose_sink(None, true, true, false), Sink::Stdout);
    }
}
