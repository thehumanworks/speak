//! Incremental playback of the canonical PCM WAV stream emitted by `--stdout`.
//!
//! This is the client half of remote desktop playback:
//! `ssh -T host 'speak --stdout' | speak --play-stdin`.

use std::io::{Read, Write};

use anyhow::{bail, Context, Result};

use crate::player::StreamingPlayer;

const WAV_HEADER_LEN: usize = 44;
const STREAMING_LENGTH: u32 = u32::MAX;
const READ_BUFFER_BYTES: usize = 32 * 1024;
const MIN_SAMPLE_RATE: u32 = 8_000;
const MAX_SAMPLE_RATE: u32 = 384_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PcmWavSpec {
    sample_rate: u32,
    data_len: Option<u64>,
}

/// Read a canonical mono PCM16 WAV from stdin and play it through the local
/// output device without buffering the complete stream.
pub fn play_stdin(verbose: bool) -> Result<()> {
    let mut input = std::io::stdin().lock();
    let mut header = [0u8; WAV_HEADER_LEN];
    input
        .read_exact(&mut header)
        .context("could not read the WAV header from stdin")?;
    let spec = parse_header(&header)?;

    let player = StreamingPlayer::new(spec.sample_rate)
        .context("could not open a local audio output device for --play-stdin")?;
    let mut remaining = spec.data_len;
    let mut buffer = [0u8; READ_BUFFER_BYTES];
    let mut carry: Option<u8> = None;
    let mut total_samples = 0u64;

    loop {
        let limit = match remaining {
            Some(0) => break,
            Some(bytes) => usize::try_from(bytes.min(buffer.len() as u64)).unwrap_or(buffer.len()),
            None => buffer.len(),
        };
        let read = input
            .read(&mut buffer[..limit])
            .context("failed while reading WAV samples from stdin")?;
        if read == 0 {
            if remaining.is_some_and(|bytes| bytes > 0) {
                bail!("WAV stream ended before its declared data length");
            }
            break;
        }
        if let Some(bytes) = &mut remaining {
            *bytes = bytes.saturating_sub(read as u64);
        }

        let mut samples = Vec::with_capacity((read + usize::from(carry.is_some())) / 2);
        let mut offset = 0usize;
        if let Some(first) = carry.take() {
            let second = buffer[0];
            samples.push(i16_to_f32(i16::from_le_bytes([first, second])));
            offset = 1;
        }
        while offset + 1 < read {
            samples.push(i16_to_f32(i16::from_le_bytes([
                buffer[offset],
                buffer[offset + 1],
            ])));
            offset += 2;
        }
        if offset < read {
            carry = Some(buffer[offset]);
        }

        total_samples += samples.len() as u64;
        player.push(&samples);
        player.wait_until_buffer_below(spec.sample_rate as usize * 20);
        if player.is_stopped() {
            bail!("local audio output device stopped during streamed playback");
        }
    }

    if carry.is_some() {
        bail!("WAV sample data ended on an incomplete 16-bit sample");
    }

    player.finish_and_wait();
    if verbose {
        let seconds = total_samples as f64 / spec.sample_rate as f64;
        let mut stderr = std::io::stderr().lock();
        writeln!(stderr, "Played {seconds:.1}s from stdin.").ok();
    }
    Ok(())
}

fn parse_header(header: &[u8; WAV_HEADER_LEN]) -> Result<PcmWavSpec> {
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        bail!("stdin is not a RIFF/WAVE stream");
    }
    if &header[12..16] != b"fmt " || u32_at(header, 16) != 16 {
        bail!("only canonical 44-byte PCM WAV headers are supported");
    }
    let audio_format = u16_at(header, 20);
    let channels = u16_at(header, 22);
    let sample_rate = u32_at(header, 24);
    let block_align = u16_at(header, 32);
    let bits_per_sample = u16_at(header, 34);
    if &header[36..40] != b"data" {
        bail!("canonical WAV data chunk was not found at byte 36");
    }
    if audio_format != 1 {
        bail!("only uncompressed PCM WAV input is supported");
    }
    if channels != 1 {
        bail!("only mono WAV input is supported; received {channels} channels");
    }
    if bits_per_sample != 16 || block_align != 2 {
        bail!("only mono 16-bit PCM WAV input is supported");
    }
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
        bail!(
            "WAV sample rate must be between {MIN_SAMPLE_RATE} and {MAX_SAMPLE_RATE} Hz; received {sample_rate}"
        );
    }
    let expected_byte_rate = sample_rate
        .checked_mul(2)
        .context("WAV sample rate overflowed the expected byte rate")?;
    if u32_at(header, 28) != expected_byte_rate {
        bail!("WAV byte rate does not match mono 16-bit PCM");
    }

    let declared = u32_at(header, 40);
    let data_len = if declared == STREAMING_LENGTH {
        None
    } else {
        Some(declared as u64)
    };
    if data_len.is_some_and(|bytes| bytes % 2 != 0) {
        bail!("WAV data length is not aligned to 16-bit samples");
    }

    Ok(PcmWavSpec {
        sample_rate,
        data_len,
    })
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn i16_to_f32(sample: i16) -> f32 {
    sample as f32 / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(data_len: u32) -> [u8; WAV_HEADER_LEN] {
        let mut h = [0u8; WAV_HEADER_LEN];
        h[0..4].copy_from_slice(b"RIFF");
        h[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        h[8..12].copy_from_slice(b"WAVE");
        h[12..16].copy_from_slice(b"fmt ");
        h[16..20].copy_from_slice(&16u32.to_le_bytes());
        h[20..22].copy_from_slice(&1u16.to_le_bytes());
        h[22..24].copy_from_slice(&1u16.to_le_bytes());
        h[24..28].copy_from_slice(&24_000u32.to_le_bytes());
        h[28..32].copy_from_slice(&48_000u32.to_le_bytes());
        h[32..34].copy_from_slice(&2u16.to_le_bytes());
        h[34..36].copy_from_slice(&16u16.to_le_bytes());
        h[36..40].copy_from_slice(b"data");
        h[40..44].copy_from_slice(&data_len.to_le_bytes());
        h
    }

    #[test]
    fn accepts_complete_and_streaming_headers() {
        assert_eq!(
            parse_header(&header(480)).unwrap(),
            PcmWavSpec {
                sample_rate: 24_000,
                data_len: Some(480)
            }
        );
        assert_eq!(parse_header(&header(u32::MAX)).unwrap().data_len, None);
    }

    #[test]
    fn rejects_non_mono_or_non_pcm_input() {
        let mut stereo = header(480);
        stereo[22..24].copy_from_slice(&2u16.to_le_bytes());
        assert!(parse_header(&stereo).is_err());

        let mut float = header(480);
        float[20..22].copy_from_slice(&3u16.to_le_bytes());
        assert!(parse_header(&float).is_err());
    }

    #[test]
    fn rejects_unreasonable_sample_rates() {
        let mut low = header(480);
        low[24..28].copy_from_slice(&1u32.to_le_bytes());
        low[28..32].copy_from_slice(&2u32.to_le_bytes());
        assert!(parse_header(&low).is_err());

        let mut high = header(480);
        high[24..28].copy_from_slice(&500_000u32.to_le_bytes());
        high[28..32].copy_from_slice(&1_000_000u32.to_le_bytes());
        assert!(parse_header(&high).is_err());
    }

    #[test]
    fn converts_pcm_extremes() {
        assert_eq!(i16_to_f32(0), 0.0);
        assert_eq!(i16_to_f32(i16::MIN), -1.0);
        assert!(i16_to_f32(i16::MAX) < 1.0);
    }
}
