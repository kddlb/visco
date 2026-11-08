//! Audio playback on a dedicated thread (the output device handle is not `Send` on macOS).

use std::{
    num::NonZero,
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

enum Cmd {
    Play { samples: Vec<f32>, channels: u16, rate: u32, start_frame: f64 },
    Stop,
}

#[derive(Default, Clone, Copy)]
struct Status {
    playing: bool,
    /// Play commands sent but not yet picked up by the audio thread.
    pending: u32,
    /// Current position in frames (samples per channel) of the original signal.
    position: f64,
}

/// Open the default output device.
///
/// rodio 0.23 asks for a fixed ~50 ms buffer rounded to a power of two (2048 frames at 48 kHz),
/// which devices with small buffer limits reject — AirPods allow at most 960 frames. Clamp the
/// request to the device's supported range, then fall back to the OS default and rodio's own logic.
fn open_sink() -> Result<rodio::MixerDeviceSink, String> {
    use rodio::cpal::{self, traits::{DeviceTrait, HostTrait}};
    let device = cpal::default_host().default_output_device().ok_or("no output device")?;
    let config = device.default_output_config().map_err(|e| format!("output config: {e}"))?;
    let want = (config.sample_rate() / 20).next_power_of_two();
    let size = match *config.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => want.clamp(min, max),
        cpal::SupportedBufferSize::Unknown => want,
    };
    let attempt = |b: cpal::BufferSize| {
        rodio::DeviceSinkBuilder::from_device(device.clone()).map(|d| d.with_buffer_size(b)).and_then(|d| d.open_stream())
    };
    attempt(cpal::BufferSize::Fixed(size))
        .or_else(|_| attempt(cpal::BufferSize::Default))
        .or_else(|_| rodio::DeviceSinkBuilder::open_default_sink())
        .map_err(|e| {
            // Include the underlying cpal error; rodio's own message is generic.
            let mut msg = e.to_string();
            let mut src = std::error::Error::source(&e);
            while let Some(s) = src {
                msg.push_str(&format!(": {s}"));
                src = s.source();
            }
            msg
        })
}

pub struct Audio {
    tx: mpsc::Sender<Cmd>,
    status: Arc<Mutex<Status>>,
    error: Arc<Mutex<Option<String>>>,
}

impl Audio {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let status = Arc::new(Mutex::new(Status::default()));
        let error = Arc::new(Mutex::new(None));
        let (st, err) = (status.clone(), error.clone());
        thread::Builder::new()
            .name("visco-audio".into())
            .spawn(move || {
                // Kept alive while its player is playing.
                let mut _sink: Option<rodio::MixerDeviceSink>;
                let mut player: Option<rodio::Player> = None;
                let mut current = (0.0f64, 1u32); // (start frame, rate)
                loop {
                    match rx.recv_timeout(Duration::from_millis(30)) {
                        Ok(Cmd::Play { samples, channels, rate, start_frame }) => {
                            {
                                let mut s = st.lock().unwrap();
                                s.pending = s.pending.saturating_sub(1);
                            }
                            if let Some(p) = player.take() {
                                p.stop();
                            }
                            // Reopen per play so a changed default device (e.g. headphones) is picked up.
                            match open_sink() {
                                Ok(mut s) => {
                                    s.log_on_drop(false);
                                    _sink = Some(s);
                                }
                                Err(e) => {
                                    *err.lock().unwrap() = Some(format!("audio output unavailable: {e}"));
                                    st.lock().unwrap().playing = false;
                                    continue;
                                }
                            }
                            let (Some(ch), Some(r)) = (NonZero::new(channels), NonZero::new(rate)) else { continue };
                            let p = rodio::Player::connect_new(_sink.as_ref().unwrap().mixer());
                            p.append(rodio::buffer::SamplesBuffer::new(ch, r, samples));
                            current = (start_frame, rate);
                            player = Some(p);
                            *err.lock().unwrap() = None;
                        }
                        Ok(Cmd::Stop) => {
                            if let Some(p) = player.take() {
                                p.stop();
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    let mut s = st.lock().unwrap();
                    match &player {
                        Some(p) if !p.empty() => {
                            s.playing = true;
                            s.position = current.0 + p.get_pos().as_secs_f64() * current.1 as f64;
                        }
                        _ => {
                            s.playing = s.pending > 0;
                            player = None;
                        }
                    }
                }
            })
            .expect("spawn audio thread");
        Self { tx, status, error }
    }

    /// Play interleaved f32 samples; `start_frame` maps playback time back to the timeline.
    pub fn play(&self, samples: Vec<f32>, channels: u16, rate: u32, start_frame: f64) {
        {
            let mut s = self.status.lock().unwrap();
            s.playing = true;
            s.pending += 1;
            s.position = start_frame;
        }
        let _ = self.tx.send(Cmd::Play { samples, channels, rate, start_frame });
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
        self.status.lock().unwrap().playing = false;
    }

    /// `Some(position in frames)` while playing.
    pub fn position(&self) -> Option<f64> {
        let s = *self.status.lock().unwrap();
        s.playing.then_some(s.position)
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
}
