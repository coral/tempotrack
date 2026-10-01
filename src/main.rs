use clap::Parser;
use std::{
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempotrack::{
    audio,
    cli::Args,
    config::Config,
    engine::{Engine, Event},
    output,
};

#[cfg(feature = "gui")]
mod ui;

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("TempoTrack: {error}");
            ExitCode::FAILURE
        }
    }
}
fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    if args.list_inputs {
        let inputs = audio::list_inputs()?;
        if inputs.is_empty() {
            println!("No audio inputs found.");
        }
        for input in inputs {
            println!("{} #{}: {}", input.host, input.index, input.name);
            for format in input.configs {
                println!(
                    "  {} channels · {}–{} Hz · {} · {:?}",
                    format.channels(),
                    format.min_sample_rate(),
                    format.max_sample_rate(),
                    format.sample_format(),
                    format.buffer_size()
                );
            }
        }
        return Ok(());
    }
    if args.list_outputs {
        println!("stdout  Text tempo, level, and pulse status\nnone    Disable output");
        return Ok(());
    }
    let gui = args.wants_gui();
    let names = args.output_names(gui)?;
    let mut drivers = Vec::new();
    for name in names {
        if let Some(driver) = output::from_name(&name)? {
            drivers.push(driver);
        }
    }
    if gui {
        #[cfg(feature = "gui")]
        {
            return ui::run(args.settings, drivers);
        }
        #[cfg(not(feature = "gui"))]
        {
            return Err("GUI support is not compiled in; rebuild with --features gui".into());
        }
    }
    let config = args.settings.apply(Config::default())?;
    let running = Arc::new(AtomicBool::new(true));
    let signal = running.clone();
    ctrlc::set_handler(move || signal.store(false, Ordering::Release))?;
    let mut engine = Engine::spawn(drivers)?;
    engine.start(config)?;
    let mut failure = None;
    while running.load(Ordering::Acquire) {
        while let Some(event) = engine.event() {
            match event {
                Event::Started {
                    input,
                    rate,
                    channels,
                } => eprintln!("Input: {input} · {rate} Hz · {channels} channels. Ctrl-C to stop."),
                Event::Error(error) => {
                    failure = Some(error);
                    running.store(false, Ordering::Release);
                }
                Event::OutputError(name) => {
                    failure = Some(format!("output {name} failed"));
                    running.store(false, Ordering::Release);
                }
            }
        }
        let _ = engine.latest();
        if engine.is_finished() {
            failure = Some("engine worker stopped unexpectedly".into());
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    engine.shutdown()?;
    if let Some(error) = failure {
        return Err(error.into());
    }
    Ok(())
}
