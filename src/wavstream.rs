//! Incremental WAV streaming to stdout.
//!
//! Rather than synthesizing the whole document and emitting one finished WAV,
//! this writes a WAV header up front and then streams each chunk's PCM as it is
//! synthesized, flushing after every chunk so a downstream player (e.g.
//! `speak --stdout | ffplay -`) hears audio as it arrives.
//!
//! The catch is the WAV header carries the total data length, which we don't
//! know until synthesis finishes. When stdout is a seekable file (`speak
//! --stdout > out.wav`) we seek back and patch the real sizes, producing a fully
//! correct file. When stdout is a true pipe we can't seek, so we emit the
//! conventional streaming sentinel length (`0xFFFFFFFF`), which streaming
//! players read until end-of-stream.

use std::io::Write;

use anyhow::{Context, Result};
use speak_core::{Engine, SynthesisRequest};

use crate::player::StreamingPlayer;

/// Build a 44-byte canonical PCM WAV header.
fn wav_header(sample_rate: u32, channels: u16, bits: u16, data_len: u32) -> [u8; 44] {
    let bytes_per_sample = (bits / 8) as u32;
    let byte_rate = sample_rate * channels as u32 * bytes_per_sample;
    let block_align = channels * bits / 8;
    // For the streaming sentinel (unknown length) keep the RIFF size at the same
    // sentinel rather than letting `36 + 0xFFFFFFFF` wrap to a nonsensical 35.
    let riff_size = if data_len == u32::MAX {
        u32::MAX
    } else {
        36u32.wrapping_add(data_len)
    };

    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&riff_size.to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&block_align.to_le_bytes());
    h[34..36].copy_from_slice(&bits.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// Clamp and convert a float PCM sample to signed 16-bit.
fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Stream the synthesized audio as a WAV byte stream to stdout, chunk by chunk.
/// If `player` is provided, also push each chunk to it for simultaneous
/// playback.
#[cfg(unix)]
pub fn stream_to_stdout(
    engine: &mut Engine,
    voice: &str,
    request: &SynthesisRequest,
    player: Option<&StreamingPlayer>,
    verbose: bool,
) -> Result<()> {
    use std::io::{BufWriter, IsTerminal, Seek, SeekFrom};
    use std::mem::ManuallyDrop;
    use std::os::unix::io::FromRawFd;

    let sample_rate = engine.sample_rate();

    // Wrap fd 1 without taking ownership, so dropping it does not close stdout.
    let mut file = unsafe { ManuallyDrop::new(std::fs::File::from_raw_fd(1)) };
    // We can only patch the header in place if stdout is seekable AND not in
    // append mode: with O_APPEND every write atomically jumps to EOF, so a
    // seek-back would append the size bytes instead of overwriting them.
    let seekable = file.seek(SeekFrom::Current(0)).is_ok() && !fd_is_append(1);

    // Always lay down the streaming sentinel sizes first. For a pipe this is the
    // final header; for a seekable file it means an interrupted run still plays
    // (lenient players read to EOF) instead of claiming zero data — we narrow it
    // to exact sizes only after a successful run.
    let mut data_bytes: u64 = 0;
    let show_progress = std::io::stderr().is_terminal();

    {
        let mut writer = BufWriter::new(&mut *file);
        writer
            .write_all(&wav_header(sample_rate, 1, 16, u32::MAX))
            .context("failed to write WAV header to stdout")?;

        engine.speak_stream(voice, request, |chunk| {
            let gap = (chunk.gap_after * sample_rate as f32) as usize;
            let mut buf: Vec<u8> = Vec::with_capacity((chunk.audio.samples.len() + gap) * 2);
            for &s in &chunk.audio.samples {
                buf.extend_from_slice(&to_i16(s).to_le_bytes());
            }
            buf.extend(std::iter::repeat_n(0u8, gap * 2));
            writer
                .write_all(&buf)
                .context("failed to write audio to stdout")?;
            // Flush so a streaming consumer gets each chunk as it lands.
            writer.flush().ok();
            data_bytes += buf.len() as u64;

            // Mirror the playback paths' bounded lookahead so the player's queue
            // does not grow without limit when stdout drains faster than real
            // time (e.g. redirected to a file). Stop feeding a dead device.
            if let Some(player) = player {
                if !player.is_stopped() {
                    player.push(&chunk.audio.samples);
                    player.push_silence(gap);
                    player.wait_until_buffer_below(sample_rate as usize * 20);
                }
            }
            if show_progress {
                eprint!("\rStreaming… chunk {}/{}", chunk.index + 1, chunk.total);
                let _ = std::io::stderr().flush();
            }
            Ok(())
        })?;

        writer.flush().ok();
    }

    if show_progress {
        eprint!("\r\x1b[K");
        let _ = std::io::stderr().flush();
    }

    // Patch real sizes only when seekable and the data fits in WAV's u32 size
    // fields; otherwise leave the self-consistent 0xFFFFFFFF sentinel rather
    // than writing a wrapped, structurally invalid RIFF size.
    let patched = seekable && data_bytes <= u32::MAX as u64;
    if patched {
        let data_len = data_bytes as u32;
        let riff_size = 36u32.wrapping_add(data_len);
        file.seek(SeekFrom::Start(4))
            .context("failed to seek stdout to patch WAV header")?;
        file.write_all(&riff_size.to_le_bytes())?;
        file.seek(SeekFrom::Start(40))?;
        file.write_all(&data_len.to_le_bytes())?;
        file.flush().ok();
    }

    if verbose {
        let secs = data_bytes as f64 / 2.0 / sample_rate as f64;
        let mode = if patched {
            "seekable: header patched"
        } else if seekable {
            "exceeds WAV 4 GiB limit: streaming header kept"
        } else {
            "pipe: streaming header"
        };
        eprintln!("Streamed {secs:.1}s of audio to stdout ({mode}).");
    }
    Ok(())
}

/// Whether the file descriptor was opened in append mode (`O_APPEND`), in which
/// case seeks do not reposition writes and the header cannot be patched.
#[cfg(unix)]
fn fd_is_append(fd: std::os::unix::io::RawFd) -> bool {
    // SAFETY: F_GETFL only reads the descriptor's status flags; no memory is
    // touched and the fd is not consumed.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    flags >= 0 && (flags & libc::O_APPEND) != 0
}

/// Non-Unix fallback: synthesize fully, then write one complete WAV. (The
/// header-patching path relies on a Unix file descriptor for stdout.)
#[cfg(not(unix))]
pub fn stream_to_stdout(
    engine: &mut Engine,
    voice: &str,
    request: &SynthesisRequest,
    player: Option<&StreamingPlayer>,
    verbose: bool,
) -> Result<()> {
    let audio = engine.speak(voice, request)?;
    let bytes = audio.to_wav_bytes()?;
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes)
        .context("failed to write WAV to stdout")?;
    out.flush().ok();
    if let Some(player) = player {
        player.push(&audio.samples);
    }
    if verbose {
        eprintln!("Wrote {:.1}s of audio to stdout.", audio.duration_secs());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(h: &[u8; 44], offset: usize) -> u32 {
        u32::from_le_bytes(h[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn header_has_correct_fixed_fields() {
        let h = wav_header(44100, 1, 16, 1000);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(&h[36..40], b"data");
        assert_eq!(u32_at(&h, 24), 44100); // sample rate
        assert_eq!(u32_at(&h, 28), 88200); // byte rate = 44100 * 1 * 2
        assert_eq!(u16::from_le_bytes([h[34], h[35]]), 16); // bits per sample
    }

    #[test]
    fn header_sizes_match_data_length() {
        let h = wav_header(44100, 1, 16, 1000);
        assert_eq!(u32_at(&h, 40), 1000); // data size
        assert_eq!(u32_at(&h, 4), 1036); // RIFF size = 36 + data
    }

    #[test]
    fn streaming_sentinel_keeps_both_sizes_maxed() {
        // An unknown length must not wrap the RIFF size to a nonsensical value.
        let h = wav_header(44100, 1, 16, u32::MAX);
        assert_eq!(u32_at(&h, 4), u32::MAX);
        assert_eq!(u32_at(&h, 40), u32::MAX);
    }

    #[test]
    fn to_i16_clamps_out_of_range() {
        assert_eq!(to_i16(0.0), 0);
        assert_eq!(to_i16(2.0), 32767);
        assert_eq!(to_i16(-2.0), -32767);
    }
}
