//! Deterministic offline driver. PCM is mono, little-endian f32; no audio device or wall clock.
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    path::PathBuf,
};
use tempotrack::{
    backend::{self, Estimate},
    config::{Config, Tracking},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    pcm: Option<PathBuf>,
    #[arg(long, conflicts_with = "pcm")]
    replay: Option<PathBuf>,
    #[arg(long)]
    guide: Option<PathBuf>,
    #[arg(long, default_value_t = 44100)]
    rate: u32,
    #[arg(long, value_enum)]
    tracking: Tracking,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=3))]
    model: u8,
}
#[derive(Serialize, Deserialize)]
struct Row {
    time: f64,
    estimate: Estimate,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if let Some(path) = args.replay {
        let input = BufReader::new(File::open(path)?);
        let mut output = BufWriter::new(io::stdout().lock());
        let guide: Vec<Row> = if let Some(path) = args.guide {
            BufReader::new(File::open(path)?)
                .lines()
                .map(|l| Ok(serde_json::from_str(&l?)?))
                .collect::<Result<_, Box<dyn std::error::Error>>>()?
        } else {
            Vec::new()
        };
        let mut index = 0;
        let mut advisory = backend::TempoAdvisor::new(4);
        let config = Config::default();
        let mut clock = if args.tracking == Tracking::Pulseweave {
            tempotrack::clock::BeatClock::new(tempotrack::config::ATOM_SUBDIVISION)
        } else {
            tempotrack::clock::BeatClock::for_config(&config)
        };
        for line in input.lines() {
            let row: Row = serde_json::from_str(&line?)?;
            while index < guide.len() && guide[index].time <= row.time {
                advisory.update(guide[index].estimate, guide[index].time);
                index += 1;
            }
            let estimate = clock.update(row.estimate, row.time, advisory.grid(row.time));
            serde_json::to_writer(
                &mut output,
                &Row {
                    time: row.time,
                    estimate,
                },
            )?;
            writeln!(output)?;
        }
        output.flush()?;
        return Ok(());
    }
    let config = Config::default();
    config.validate()?;
    let mut backend = backend::create_raw_for_model(&config, args.rate, args.tracking, args.model)?;
    let mut input = BufReader::new(File::open(
        args.pcm.ok_or("--pcm or --replay is required")?,
    )?);
    let mut output = BufWriter::new(io::stdout().lock());
    let mut bytes = [0_u8; 128 * 4];
    let mut frame = 0_u64;
    let mut estimates = Vec::with_capacity(16);
    loop {
        let mut len = 0;
        while len < bytes.len() {
            let n = input.read(&mut bytes[len..])?;
            if n == 0 {
                break;
            }
            len += n;
        }
        if len == 0 {
            break;
        }
        if len % 4 != 0 {
            return Err("truncated f32 PCM".into());
        }
        let mut samples = [0_f32; 128];
        for (sample, bytes) in samples.iter_mut().zip(bytes[..len].as_chunks::<4>().0) {
            *sample = f32::from_le_bytes(*bytes);
        }
        frame += (len / 4) as u64;
        estimates.clear();
        backend.process_each(&samples[..len / 4], &mut |estimate| {
            estimates.push(estimate)
        })?;
        for estimate in &estimates {
            serde_json::to_writer(
                &mut output,
                &Row {
                    time: frame as f64 / args.rate as f64,
                    estimate: *estimate,
                },
            )?;
            writeln!(output)?;
        }
    }
    output.flush()?;
    Ok(())
}
