//! Device setup and the bounded callback-to-worker handoff.
use crate::{Error, config::Config};
use cpal::{
    FromSample, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub const BLOCK: usize = 256;

#[derive(Debug, Clone)]
pub struct InputDevice {
    pub host: String,
    pub name: String,
    pub id: String,
    pub index: usize,
    pub is_default: bool,
    pub configs: Vec<cpal::SupportedStreamConfigRange>,
}
impl std::fmt::Display for InputDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} · {} #{}", self.name, self.host, self.index)
    }
}
impl PartialEq for InputDevice {
    fn eq(&self, other: &Self) -> bool {
        self.host == other.host && self.id == other.id
    }
}
impl Eq for InputDevice {}

pub fn hosts() -> Vec<String> {
    cpal::available_hosts()
        .iter()
        .map(|h| h.name().to_owned())
        .collect()
}
pub fn default_host_name() -> String {
    cpal::default_host().id().name().to_owned()
}
fn host(name: Option<&str>) -> Result<cpal::Host, Error> {
    match name {
        None => Ok(cpal::default_host()),
        Some(name) => {
            let id = cpal::available_hosts()
                .into_iter()
                .find(|id| id.name() == name)
                .ok_or_else(|| {
                    Error::Config(format!(
                        "unknown audio host {name:?}; available: {}",
                        hosts().join(", ")
                    ))
                })?;
            cpal::host_from_id(id).map_err(|e| Error::Config(e.to_string()))
        }
    }
}
pub fn list_inputs() -> Result<Vec<InputDevice>, Error> {
    let mut result = Vec::new();
    for id in cpal::available_hosts() {
        let host = cpal::host_from_id(id).map_err(|e| Error::Config(e.to_string()))?;
        let default = host.default_input_device();
        for (index, device) in host.input_devices()?.enumerate() {
            result.push(InputDevice {
                host: id.name().to_owned(),
                name: device.description()?.name().to_owned(),
                id: device.id()?.to_string(),
                index,
                is_default: default.as_ref() == Some(&device),
                configs: device.supported_input_configs()?.collect(),
            });
        }
    }
    Ok(result)
}

/// Exact, unique matching is shared by the GUI/CLI and independently testable.
pub fn unique_name(names: &[String], selected: &str) -> Result<usize, Error> {
    let mut matches = names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.as_str() == selected);
    let Some((index, _)) = matches.next() else {
        return Err(Error::Config(format!(
            "input {selected:?} was not found; use --list-inputs"
        )));
    };
    if matches.next().is_some() {
        return Err(Error::Config(format!(
            "input {selected:?} is ambiguous; use --host and --input-index"
        )));
    }
    Ok(index)
}

pub struct PreparedInput {
    pub device: cpal::Device,
    pub config: cpal::StreamConfig,
    pub format: SampleFormat,
    pub name: String,
}
pub fn prepare(config: &Config) -> Result<PreparedInput, Error> {
    config.validate()?;
    let host = host(config.host.as_deref())?;
    let devices: Vec<_> = host.input_devices()?.collect();
    let device = if let Some(id) = &config.input_id {
        devices
            .into_iter()
            .find(|d| d.id().is_ok_and(|v| v.to_string() == *id))
            .ok_or_else(|| {
                Error::Config("saved input is unavailable; select an input in Settings".into())
            })?
    } else if let Some(index) = config.input_index {
        devices
            .into_iter()
            .nth(index)
            .ok_or_else(|| Error::Config("input index is unavailable; use --list-inputs".into()))?
    } else if let Some(name) = &config.input {
        let names: Result<Vec<_>, _> = devices
            .iter()
            .map(|d| d.description().map(|v| v.name().to_owned()))
            .collect();
        let index = unique_name(&names?, name)?;
        devices
            .into_iter()
            .nth(index)
            .ok_or_else(|| Error::Config("input disappeared".into()))?
    } else {
        host.default_input_device()
            .ok_or_else(|| Error::Config("no default audio input; select an input".into()))?
    };
    let default = device.default_input_config()?;
    let rate = default.sample_rate();
    let mut candidates: Vec<_> = device
        .supported_input_configs()?
        .filter(|range| {
            range.min_sample_rate() <= rate
                && range.max_sample_rate() >= rate
                && range.channels() >= config.channel.unwrap_or(1)
                && range.channels() <= 64
                && pcm_format(range.sample_format())
        })
        .collect();
    candidates.sort_by_key(|range| {
        (
            range.channels() != default.channels(),
            range.sample_format() != default.sample_format(),
            range.channels(),
        )
    });
    let supported = candidates
        .first()
        .ok_or_else(|| {
            Error::Config(
                "input does not support the selected channels at its default sample rate".into(),
            )
        })?
        .with_sample_rate(rate);
    if !(8_000..=192_000).contains(&rate) {
        return Err(Error::Config("input rate must be 8000..192000 Hz".into()));
    }
    let stream_config: cpal::StreamConfig = supported.into();
    let name = device.description()?.name().to_owned();
    Ok(PreparedInput {
        device,
        config: stream_config,
        format: supported.sample_format(),
        name,
    })
}
fn pcm_format(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::F32
            | SampleFormat::F64
            | SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I24
            | SampleFormat::I32
            | SampleFormat::I64
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U24
            | SampleFormat::U32
            | SampleFormat::U64
    )
}

#[derive(Clone, Copy)]
pub struct AudioBlock {
    pub source_frame: u64,
    pub captured_at: Instant,
    pub samples: [f32; BLOCK],
}
#[derive(Default)]
pub struct CaptureStats {
    pub dropped_frames: AtomicU64,
    pub device_errors: AtomicU64,
    pub discontinuities: AtomicU64,
    pub scheduling_warnings: AtomicU64,
    failure: AtomicUsize,
}
const FATAL_KINDS: &[cpal::ErrorKind] = &[
    cpal::ErrorKind::DeviceBusy,
    cpal::ErrorKind::DeviceNotAvailable,
    cpal::ErrorKind::HostUnavailable,
    cpal::ErrorKind::InvalidInput,
    cpal::ErrorKind::PermissionDenied,
    cpal::ErrorKind::ResourceExhausted,
    cpal::ErrorKind::StreamInvalidated,
    cpal::ErrorKind::UnsupportedConfig,
    cpal::ErrorKind::UnsupportedOperation,
    cpal::ErrorKind::BackendError,
];
impl CaptureStats {
    /// Store only a copyable error category; never format/allocate in the callback.
    pub fn record_error(&self, kind: cpal::ErrorKind) {
        match kind {
            cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::Xrun => {
                self.discontinuities.fetch_add(1, Ordering::Relaxed);
            }
            cpal::ErrorKind::RealtimeDenied => {
                self.scheduling_warnings.fetch_add(1, Ordering::Relaxed);
            }
            kind => {
                let index = FATAL_KINDS
                    .iter()
                    .position(|candidate| *candidate == kind)
                    .unwrap_or(FATAL_KINDS.len() - 1);
                self.failure.store(index + 1, Ordering::Relaxed);
                self.device_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    pub fn failure(&self) -> Option<cpal::ErrorKind> {
        self.failure
            .load(Ordering::Relaxed)
            .checked_sub(1)
            .and_then(|index| FATAL_KINDS.get(index).copied())
    }
}

/// All storage is created before starting capture. Handles split channel frames too.
pub struct CaptureWriter {
    producer: Producer<AudioBlock>,
    stats: Arc<CaptureStats>,
    rate: u32,
    channels: u16,
    selected: Option<u16>,
    channel: u16,
    sum: f64,
    frame: u64,
    filled: usize,
    block: AudioBlock,
}
impl CaptureWriter {
    pub fn new(
        rate: u32,
        channels: u16,
        selected: Option<u16>,
        capacity: usize,
    ) -> Result<(Self, Consumer<AudioBlock>, Arc<CaptureStats>), Error> {
        if !(8_000..=192_000).contains(&rate)
            || !(1..=64).contains(&channels)
            || selected.is_some_and(|c| c == 0 || c > channels)
            || capacity == 0
        {
            return Err(Error::Config("invalid capture queue configuration".into()));
        }
        let (producer, consumer) = RingBuffer::new(capacity);
        let stats = Arc::new(CaptureStats::default());
        Ok((
            Self {
                producer,
                stats: stats.clone(),
                rate,
                channels,
                selected,
                channel: 0,
                sum: 0.,
                frame: 0,
                filled: 0,
                block: AudioBlock {
                    source_frame: 0,
                    captured_at: Instant::now(),
                    samples: [0.; BLOCK],
                },
            },
            consumer,
            stats,
        ))
    }
    pub fn push<T: SizedSample>(&mut self, data: &[T], captured_at: Instant)
    where
        f32: FromSample<T>,
    {
        let mut callback_frame = 0u64;
        for &sample in data {
            if self.channel == 0 && self.filled == 0 {
                self.block.source_frame = self.frame;
                self.block.captured_at =
                    captured_at + Duration::from_secs_f64(callback_frame as f64 / self.rate as f64);
            }
            let sample = f32::from_sample_(sample);
            let sample = if sample.is_finite() {
                sample.clamp(-1., 1.)
            } else {
                0.
            };
            if self.selected.is_none_or(|c| c == self.channel + 1) {
                self.sum += f64::from(sample);
            }
            self.channel += 1;
            if self.channel != self.channels {
                continue;
            }
            self.block.samples[self.filled] = (self.sum
                / if self.selected.is_some() {
                    1.
                } else {
                    f64::from(self.channels)
                }) as f32;
            self.sum = 0.;
            self.channel = 0;
            self.frame += 1;
            callback_frame += 1;
            self.filled += 1;
            if self.filled == BLOCK {
                if self.producer.push(self.block).is_err() {
                    self.stats
                        .dropped_frames
                        .fetch_add(BLOCK as u64, Ordering::Relaxed);
                }
                self.filled = 0;
            }
        }
    }
}

pub struct Capture {
    pub stream: cpal::Stream,
    pub audio: Consumer<AudioBlock>,
    pub stats: Arc<CaptureStats>,
}
pub fn build(prepared: &PreparedInput, channel: Option<u16>) -> Result<Capture, Error> {
    let capacity = (prepared.config.sample_rate as usize / 4).div_ceil(BLOCK);
    let (writer, audio, stats) = CaptureWriter::new(
        prepared.config.sample_rate,
        prepared.config.channels,
        channel,
        capacity,
    )?;
    macro_rules! stream {
        ($t:ty) => {
            build_typed::<$t>(prepared, writer, stats.clone())?
        };
    }
    let stream = match prepared.format {
        SampleFormat::F32 => stream!(f32),
        SampleFormat::F64 => stream!(f64),
        SampleFormat::I8 => stream!(i8),
        SampleFormat::I16 => stream!(i16),
        SampleFormat::I24 => stream!(cpal::I24),
        SampleFormat::I32 => stream!(i32),
        SampleFormat::I64 => stream!(i64),
        SampleFormat::U8 => stream!(u8),
        SampleFormat::U16 => stream!(u16),
        SampleFormat::U24 => stream!(cpal::U24),
        SampleFormat::U32 => stream!(u32),
        SampleFormat::U64 => stream!(u64),
        format => return Err(Error::Config(format!("unsupported input format: {format}"))),
    };
    Ok(Capture {
        stream,
        audio,
        stats,
    })
}
fn build_typed<T: SizedSample>(
    prepared: &PreparedInput,
    mut writer: CaptureWriter,
    stats: Arc<CaptureStats>,
) -> Result<cpal::Stream, Error>
where
    f32: FromSample<T>,
{
    Ok(prepared.device.build_input_stream(
        prepared.config,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            let now = Instant::now();
            let stamps = info.timestamp();
            let delay = stamps.callback.duration_since(stamps.capture);
            writer.push(data, now.checked_sub(delay).unwrap_or(now));
        },
        move |error| stats.record_error(error.kind()),
        Some(Duration::from_secs(5)),
    )?)
}
