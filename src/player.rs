//! Real-time streaming audio playback.
//!
//! A [`StreamingPlayer`] owns an output stream and a shared sample queue. The
//! synthesis loop pushes mono PCM (at the model's sample rate) into the queue
//! as each chunk is generated; the audio device's callback drains it, resampling
//! to the device rate on the fly and fanning the mono signal across the device's
//! channels. Because synthesis runs several times faster than real time, the
//! queue stays ahead of playback and audio is gapless after the first chunk —
//! the listener hears the opening words while the rest is still being made.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

/// State shared between the producer (synthesis loop) and the audio callback.
struct Shared {
    /// Mono samples at the source (model) sample rate, awaiting playback.
    queue: Mutex<VecDeque<f32>>,
    /// Set once the producer has pushed everything it will ever push.
    finished: AtomicBool,
    /// Set if the device stream errors out (disconnect, fatal xrun, ...). Once
    /// set, the audio callback will not run again, so waiters must give up
    /// rather than block on a drain that can never happen.
    stopped: AtomicBool,
    /// Signaled when the queue is fully drained (by the audio callback) or the
    /// stream has stopped (by the error callback), so a waiter can unblock.
    drained: (Mutex<bool>, Condvar),
}

pub struct StreamingPlayer {
    shared: Arc<Shared>,
    // The stream must stay alive for playback to continue; dropping it stops
    // the device. It is `!Send`, so the player stays on the thread that built it.
    _stream: cpal::Stream,
}

impl StreamingPlayer {
    /// Open the default output device and start a stream that plays whatever is
    /// pushed into the queue. `source_rate` is the sample rate of the audio that
    /// will be pushed (the model's rate); it is resampled to the device rate.
    pub fn new(source_rate: u32) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("no default audio output device")?;
        let supported = device
            .default_output_config()
            .context("no default output config for the audio device")?;

        let device_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.config();

        let shared = Arc::new(Shared {
            // Reserve generously so the producer's `extend` stays within
            // capacity and never reallocates the backlog while the real-time
            // audio callback is waiting on the same lock.
            queue: Mutex::new(VecDeque::with_capacity(source_rate as usize * 45)),
            finished: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            drained: (Mutex::new(false), Condvar::new()),
        });

        // Input samples consumed per output frame. 1.0 when rates match.
        let step = source_rate as f64 / device_rate as f64;

        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                build_stream::<f32>(&device, &config, shared.clone(), channels, step)
            }
            cpal::SampleFormat::I16 => {
                build_stream::<i16>(&device, &config, shared.clone(), channels, step)
            }
            cpal::SampleFormat::U16 => {
                build_stream::<u16>(&device, &config, shared.clone(), channels, step)
            }
            other => anyhow::bail!("unsupported audio output sample format: {other:?}"),
        }?;
        stream.play().context("failed to start audio playback")?;

        Ok(Self {
            shared,
            _stream: stream,
        })
    }

    /// Queue mono samples (at the source sample rate) for playback.
    pub fn push(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let mut queue = self.shared.queue.lock().unwrap();
        queue.extend(samples.iter().copied());
    }

    /// Queue `n` samples of silence (a gap between chunks).
    pub fn push_silence(&self, n: usize) {
        if n == 0 {
            return;
        }
        let mut queue = self.shared.queue.lock().unwrap();
        queue.extend(std::iter::repeat_n(0.0, n));
    }

    /// Whether the audio stream has reported a fatal error and stopped. Producers
    /// should stop pushing and abort once this is true.
    pub fn is_stopped(&self) -> bool {
        self.shared.stopped.load(Ordering::Acquire)
    }

    /// Block until the queued backlog falls below `max_samples`, so the producer
    /// does not race ahead and buffer the entire document in memory. Returns
    /// early if the stream has stopped (otherwise a dead device, which no longer
    /// drains the queue, would wedge the producer here forever).
    pub fn wait_until_buffer_below(&self, max_samples: usize) {
        loop {
            if self.is_stopped() {
                return;
            }
            let len = self.shared.queue.lock().unwrap().len();
            if len <= max_samples {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Signal that no more samples will be pushed, then block until everything
    /// queued has been played out (or the stream stops on an error). The wait is
    /// bounded and re-checks `stopped`, so a device error can never hang the CLI.
    pub fn finish_and_wait(&self) {
        self.shared.finished.store(true, Ordering::Release);
        let (lock, cvar) = &self.shared.drained;
        let mut done = lock.lock().unwrap();
        while !*done && !self.is_stopped() {
            let (guard, _) = cvar.wait_timeout(done, Duration::from_millis(250)).unwrap();
            done = guard;
        }
        // Give the device a moment to play out its own internal buffer before
        // the stream is dropped, so the final syllable is not clipped.
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Build an output stream for a concrete device sample type, draining the shared
/// queue with linear-interpolation resampling from the source rate to the device
/// rate and writing each mono sample to every channel.
fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Arc<Shared>,
    channels: usize,
    step: f64,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    // Fractional read position between the two front samples of the queue.
    let mut frac: f64 = 0.0;
    let err_shared = shared.clone();

    let stream = device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
                let finished = shared.finished.load(Ordering::Acquire);
                let mut queue = shared.queue.lock().unwrap();
                let frames = out.len() / channels.max(1);
                for f in 0..frames {
                    let sample = next_sample(&mut queue, &mut frac, step, finished);
                    let value = T::from_sample(sample);
                    let base = f * channels;
                    for c in 0..channels {
                        out[base + c] = value;
                    }
                }
                let empty = queue.is_empty();
                drop(queue);
                if empty && finished {
                    let (lock, cvar) = &shared.drained;
                    let mut done = lock.lock().unwrap();
                    if !*done {
                        *done = true;
                        cvar.notify_all();
                    }
                }
            },
            move |err| {
                eprintln!("audio output error: {err}");
                // The stream is dead: the data callback will not run again, so
                // mark stopped and wake any waiter in finish_and_wait, otherwise
                // it would block forever on a drain that can never complete.
                err_shared.stopped.store(true, Ordering::Release);
                let (lock, cvar) = &err_shared.drained;
                let mut done = lock.lock().unwrap();
                *done = true;
                cvar.notify_all();
            },
            None,
        )
        .context("failed to build audio output stream")?;
    Ok(stream)
}

/// Produce the next output sample, advancing the fractional read position and
/// popping consumed input samples.
///
/// With two or more queued samples it interpolates between them. With fewer it
/// depends on `finished`: at end of stream (`finished`) it plays out the final
/// sample then silence; mid-stream (a transient producer stall) it returns
/// silence WITHOUT advancing `frac` or dropping the lone sample, so when the
/// producer catches up interpolation resumes in phase rather than clicking.
fn next_sample(queue: &mut VecDeque<f32>, frac: &mut f64, step: f64, finished: bool) -> f32 {
    if queue.len() >= 2 {
        let a = queue[0];
        let b = queue[1];
        let s = a + (b - a) * (*frac as f32);
        *frac += step;
        while *frac >= 1.0 && queue.len() >= 2 {
            queue.pop_front();
            *frac -= 1.0;
        }
        s
    } else if finished && queue.len() == 1 {
        // The true tail: play the last sample, then let the queue empty.
        let s = queue[0];
        *frac += step;
        if *frac >= 1.0 {
            queue.pop_front();
            *frac = 0.0;
        }
        s
    } else {
        // Empty, or starved before the stream is finished: emit silence and
        // hold position so a refill resumes cleanly.
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Drain at end of stream (finished = true), so the tail plays out.
    fn drain(samples: &[f32], step: f64, count: usize) -> Vec<f32> {
        let mut queue: VecDeque<f32> = samples.iter().copied().collect();
        let mut frac = 0.0f64;
        (0..count)
            .map(|_| next_sample(&mut queue, &mut frac, step, true))
            .collect()
    }

    #[test]
    fn passthrough_when_rates_match() {
        // step == 1.0: one input consumed per output, values unchanged, then
        // silence once drained.
        assert_eq!(drain(&[1.0, 2.0, 3.0], 1.0, 4), vec![1.0, 2.0, 3.0, 0.0]);
    }

    #[test]
    fn upsampling_interpolates_between_samples() {
        // step == 0.5 (e.g. 24k -> 48k): a midpoint appears between the inputs.
        let out = drain(&[0.0, 1.0], 0.5, 3);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[1], 0.5);
        assert_eq!(out[2], 1.0);
    }

    #[test]
    fn underrun_yields_silence() {
        assert_eq!(drain(&[], 1.0, 3), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn midstream_starvation_holds_phase_without_consuming() {
        // One sample left, more still coming (not finished): emit silence but
        // keep the sample and the phase so interpolation resumes cleanly.
        let mut queue: VecDeque<f32> = [5.0].into();
        let mut frac = 0.3f64;
        let s = next_sample(&mut queue, &mut frac, 1.0, false);
        assert_eq!(s, 0.0);
        assert_eq!(
            queue.len(),
            1,
            "the lone sample must not be dropped mid-stream"
        );
        assert_eq!(frac, 0.3, "phase must be preserved across an underrun");
    }
}
