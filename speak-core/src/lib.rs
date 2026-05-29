//! speak-core — an on-device text-to-speech SDK built on Supertonic-3 and ONNX
//! Runtime.
//!
//! The crate is deliberately UI-agnostic: it loads the ONNX models once, turns
//! text into mono PCM audio, and hands back an [`Audio`] value that the caller
//! can write to a file, encode to WAV bytes, stream over HTTP, or feed to an
//! audio device. The `speak` CLI, a Tauri app, and a web server can all sit on
//! top of the same [`Engine`].
//!
//! ```no_run
//! use speak_core::{Engine, ModelLocator, SynthesisRequest};
//!
//! let mut engine = Engine::load(ModelLocator::from_cache())?;
//! let audio = engine.speak("F1", &SynthesisRequest::new("Hello from Rust."))?;
//! audio.write_wav("hello.wav")?;
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! ## Concurrency
//!
//! [`Engine::speak`] takes `&mut self` because ONNX Runtime sessions run
//! single-threaded per session. For a web server, put the engine behind a
//! `Mutex`, or keep a small pool of engines and hand one out per request.

// Vendored upstream code: it exposes more surface than the SDK uses and tracks
// upstream's style rather than this crate's clippy profile, so silence both
// here instead of editing the vendored file.
#[allow(dead_code, clippy::all)]
mod helper;

#[cfg(feature = "download")]
mod download;

#[cfg(feature = "download")]
pub use download::ensure_models;

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};

pub use helper::{is_valid_lang, AVAILABLE_LANGS};

/// The ten built-in voices. `M*` are male, `F*` are female; each id is a
/// distinct timbre and delivery rather than an emotion setting.
pub const BUILTIN_VOICES: &[&str] = &[
    "M1", "M2", "M3", "M4", "M5", "F1", "F2", "F3", "F4", "F5",
];

/// Default model cache directory name, matching the Python `supertonic`
/// package (`~/.cache/supertonic3`).
const DEFAULT_CACHE_SUBDIR: &str = ".cache/supertonic3";

/// The model files [`Engine::load`] needs, relative to the `onnx/` directory.
pub(crate) const REQUIRED_ONNX_FILES: &[&str] = &[
    "duration_predictor.onnx",
    "text_encoder.onnx",
    "vector_estimator.onnx",
    "vocoder.onnx",
    "unicode_indexer.json",
    "tts.json",
];

/// Locates the Supertonic model assets on disk.
///
/// A complete install is a base directory containing an `onnx/` directory (the
/// four ONNX models plus `tts.json` and `unicode_indexer.json`) and a
/// `voice_styles/` directory (`M1.json` … `F5.json`). This matches the layout
/// the Python package downloads into `~/.cache/supertonic3`.
#[derive(Clone, Debug)]
pub struct ModelLocator {
    base: PathBuf,
}

impl ModelLocator {
    /// Use an explicit base directory.
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// Resolve the base directory from the environment, mirroring the Python
    /// package: `$SUPERTONIC_CACHE_DIR` if set, otherwise
    /// `$HOME/.cache/supertonic3`.
    pub fn from_cache() -> Self {
        if let Ok(dir) = std::env::var("SUPERTONIC_CACHE_DIR") {
            if !dir.is_empty() {
                return Self::new(dir);
            }
        }
        let home = std::env::var("HOME").unwrap_or_default();
        Self::new(Path::new(&home).join(DEFAULT_CACHE_SUBDIR))
    }

    /// The cache base directory (holds `onnx/` and `voice_styles/`).
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// The directory holding the four ONNX models and their JSON sidecars.
    pub fn onnx_dir(&self) -> PathBuf {
        self.base.join("onnx")
    }

    /// The directory holding the built-in voice style JSON files.
    pub fn voice_styles_dir(&self) -> PathBuf {
        self.base.join("voice_styles")
    }

    /// Model files that are expected but missing. Empty means ready to load.
    pub fn missing_files(&self) -> Vec<PathBuf> {
        let onnx = self.onnx_dir();
        REQUIRED_ONNX_FILES
            .iter()
            .map(|f| onnx.join(f))
            .filter(|p| !p.exists())
            .collect()
    }

    /// Whether all required model files are present.
    pub fn is_available(&self) -> bool {
        self.missing_files().is_empty()
    }

    /// Resolve a voice selector to a style JSON path. A built-in id (`M1`…`F5`,
    /// case-insensitive) maps into `voice_styles/`; anything else is treated as
    /// a path to a custom style JSON.
    pub fn resolve_voice(&self, voice: &str) -> Result<PathBuf> {
        let upper = voice.to_ascii_uppercase();
        if BUILTIN_VOICES.contains(&upper.as_str()) {
            let path = self.voice_styles_dir().join(format!("{upper}.json"));
            if !path.exists() {
                bail!(
                    "built-in voice '{upper}' not found at {}; is the model cache complete?",
                    path.display()
                );
            }
            return Ok(path);
        }

        let path = PathBuf::from(voice);
        if path.exists() {
            return Ok(path);
        }
        bail!(
            "unknown voice '{voice}': expected one of {BUILTIN_VOICES:?} or a path to a style JSON"
        );
    }
}

/// Parameters for one synthesis call. Build with [`SynthesisRequest::new`] and
/// tweak via the chainable setters; sensible defaults match the upstream
/// example (English, 8 denoising steps, speed 1.05).
#[derive(Clone, Debug)]
pub struct SynthesisRequest {
    /// Text to synthesize.
    pub text: String,
    /// ISO-ish language code (see [`AVAILABLE_LANGS`]); `"na"` for unknown.
    pub lang: String,
    /// Denoising steps. Higher is better quality but slower.
    pub steps: usize,
    /// Speed factor; 0.9–1.5 is the useful range.
    pub speed: f32,
    /// Silence (seconds) inserted between auto-split long-text chunks.
    pub silence: f32,
}

impl SynthesisRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            lang: "en".to_string(),
            steps: 8,
            speed: 1.05,
            silence: 0.3,
        }
    }

    pub fn lang(mut self, lang: impl Into<String>) -> Self {
        self.lang = lang.into();
        self
    }

    pub fn steps(mut self, steps: usize) -> Self {
        self.steps = steps;
        self
    }

    pub fn speed(mut self, speed: f32) -> Self {
        self.speed = speed;
        self
    }

    pub fn silence(mut self, silence: f32) -> Self {
        self.silence = silence;
        self
    }
}

/// Synthesized audio: mono 32-bit float PCM plus its sample rate.
#[derive(Clone, Debug)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Audio {
    /// Length of the audio in seconds.
    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.samples.len() as f32 / self.sample_rate as f32
    }

    /// Encode to an in-memory 16-bit PCM WAV (for HTTP responses, IPC, etc.).
    pub fn to_wav_bytes(&self) -> Result<Vec<u8>> {
        let spec = WavSpec {
            channels: 1,
            sample_rate: self.sample_rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec)
                .context("failed to start WAV encoder")?;
            for &sample in &self.samples {
                let clamped = sample.clamp(-1.0, 1.0);
                writer.write_sample((clamped * 32767.0) as i16)?;
            }
            writer.finalize().context("failed to finalize WAV")?;
        }
        Ok(cursor.into_inner())
    }

    /// Write a 16-bit PCM WAV file to `path`.
    pub fn write_wav(&self, path: impl AsRef<Path>) -> Result<()> {
        helper::write_wav_file(path, &self.samples, self.sample_rate as i32)
    }
}

/// Where inference runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Device {
    /// CPU-only inference. This is the default: Supertonic-3's dynamic-shape
    /// flow-matching graph is rejected by CoreML (and CPU is already faster
    /// than real-time), so GPU is opt-in rather than automatic.
    #[default]
    Cpu,
    /// Use the GPU (CoreML on macOS: GPU + Neural Engine) when available,
    /// falling back to CPU for unsupported ops or if it cannot initialize.
    Auto,
}

/// Human-readable label for the execution backend a load resolved to.
fn backend_label(use_gpu: bool) -> &'static str {
    if use_gpu && cfg!(feature = "coreml") {
        "GPU (CoreML), CPU fallback"
    } else if use_gpu {
        "CPU (built without CoreML support)"
    } else {
        "CPU"
    }
}

/// The TTS engine: four ONNX sessions loaded once and reused across calls.
pub struct Engine {
    locator: ModelLocator,
    tts: helper::TextToSpeech,
    backend: &'static str,
}

impl Engine {
    /// Load the models described by `locator` on the default [`Device`] (CPU).
    pub fn load(locator: ModelLocator) -> Result<Self> {
        Self::load_with(locator, Device::default())
    }

    /// Load the models on a specific [`Device`]. Returns an actionable error if
    /// the model cache is incomplete.
    pub fn load_with(locator: ModelLocator, device: Device) -> Result<Self> {
        let missing = locator.missing_files();
        if !missing.is_empty() {
            let list = missing
                .iter()
                .map(|p| format!("  - {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "Supertonic model files are missing under {}:\n{list}\n\nThese are normally downloaded automatically on first run. This path means downloading was disabled (--no-download, or built without the `download` feature). Re-run with downloads enabled, or set SUPERTONIC_CACHE_DIR / pass a model dir pointing at a populated cache.",
                locator.onnx_dir().display()
            );
        }

        let onnx_dir = locator.onnx_dir();
        let onnx_dir_str = onnx_dir
            .to_str()
            .context("model directory path is not valid UTF-8")?
            .to_string();

        let use_gpu = matches!(device, Device::Auto);
        let (tts, backend) = match helper::load_text_to_speech(&onnx_dir_str, use_gpu) {
            Ok(tts) => (tts, backend_label(use_gpu)),
            Err(err) if use_gpu => {
                // Belt-and-braces fallback: ONNX Runtime already falls back to
                // CPU on its own, but if GPU session creation hard-fails, retry
                // on CPU rather than giving up.
                eprintln!("GPU initialization failed ({err}); retrying on CPU");
                let tts = helper::load_text_to_speech(&onnx_dir_str, false)
                    .context("failed to load ONNX models on CPU")?;
                (tts, backend_label(false))
            }
            Err(err) => return Err(err).context("failed to load ONNX models"),
        };
        Ok(Self { locator, tts, backend })
    }

    /// Like [`Engine::load_with`], but first downloads any missing model files
    /// from Hugging Face into the cache, so a fresh machine works without a
    /// separate setup step. Requires the `download` feature.
    #[cfg(feature = "download")]
    pub fn load_or_download(locator: ModelLocator, device: Device) -> Result<Self> {
        download::ensure_models(&locator)?;
        Self::load_with(locator, device)
    }

    /// The sample rate of generated audio (Hz).
    pub fn sample_rate(&self) -> u32 {
        self.tts.sample_rate as u32
    }

    /// The locator this engine was loaded from.
    pub fn locator(&self) -> &ModelLocator {
        &self.locator
    }

    /// Human-readable label for the execution backend in use, e.g. `"CPU"` or
    /// `"GPU (CoreML), CPU fallback"`.
    pub fn backend(&self) -> &str {
        self.backend
    }

    /// Synthesize `req` with the given voice (built-in id or style JSON path).
    pub fn speak(&mut self, voice: &str, req: &SynthesisRequest) -> Result<Audio> {
        if !is_valid_lang(&req.lang) {
            bail!(
                "unsupported language '{}'; valid codes: {}",
                req.lang,
                AVAILABLE_LANGS.join(", ")
            );
        }
        let style_path = self.locator.resolve_voice(voice)?;
        let style_path_str = style_path
            .to_str()
            .context("voice style path is not valid UTF-8")?
            .to_string();
        let style = helper::load_voice_style(&[style_path_str], false)
            .with_context(|| format!("failed to load voice style '{voice}'"))?;

        let (samples, _duration) = self
            .tts
            .call(&req.text, &req.lang, &style, req.steps, req.speed, req.silence)
            .context("synthesis failed")?;

        Ok(Audio {
            samples,
            sample_rate: self.sample_rate(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_label_is_cpu_when_gpu_not_requested() {
        // CPU is reported regardless of which features are compiled in.
        assert_eq!(backend_label(false), "CPU");
    }

    #[test]
    #[cfg(feature = "coreml")]
    fn backend_label_reports_gpu_when_built_with_coreml() {
        assert_eq!(backend_label(true), "GPU (CoreML), CPU fallback");
    }

    #[test]
    #[cfg(not(feature = "coreml"))]
    fn backend_label_reports_cpu_only_without_coreml() {
        assert_eq!(backend_label(true), "CPU (built without CoreML support)");
    }
}
