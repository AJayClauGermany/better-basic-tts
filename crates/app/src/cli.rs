//! Command line interface.

use crate::{gui, headless};
use anyhow::{bail, Context, Result};
use bbtts_core::Settings;
use bbtts_platform::ipc::{self, Request};
use clap::{Args, Parser, Subcommand};
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "better-basic-tts",
    version,
    about = "A simple, local text-to-speech controller.",
    long_about = "A simple, local text-to-speech controller.\n\n\
        Run without arguments to open the control panel. The subcommands talk to the \
        running panel, so they can be bound to keys in your desktop's shortcut settings."
)]
struct Cli {
    /// Use this settings file instead of the default one.
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Read text aloud (from arguments, a file, or standard input).
    Speak(SpeakArgs),
    /// Read the text that is currently selected.
    SpeakSelection,
    /// Read the text on the clipboard.
    SpeakClipboard,
    /// Pause or resume.
    Pause,
    /// Stop reading.
    Stop,
    /// Go back one sentence.
    Previous,
    /// Skip to the next sentence.
    Next,
    /// Read slower.
    Slower,
    /// Read faster.
    Faster,
    /// Bring the control panel to the front.
    Show,
    /// List the voices of an engine.
    Voices {
        /// Engine id (see `engines`); defaults to the configured engine.
        #[arg(long)]
        engine: Option<String>,
    },
    /// List the speech engines found on this computer.
    Engines,
    /// Show how text would be split and spoken, without speaking it.
    Explain(TextSource),
}

#[derive(Args)]
struct TextSource {
    /// The text. Without it (and without --file) standard input is read.
    text: Vec<String>,
    /// Read the text from this file ("-" for standard input).
    #[arg(short, long, value_name = "FILE")]
    file: Option<PathBuf>,
}

#[derive(Args)]
struct SpeakArgs {
    #[command(flatten)]
    source: TextSource,
    /// Write a WAV file instead of playing.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
    /// Speak from this terminal and wait, without opening the control panel.
    #[arg(long)]
    no_gui: bool,
    /// Voice id (see `voices`).
    #[arg(long)]
    voice: Option<String>,
    /// Speed multiplier, 1.0 is normal.
    #[arg(long)]
    rate: Option<f32>,
    /// Engine id (see `engines`).
    #[arg(long)]
    engine: Option<String>,
}

impl TextSource {
    fn read(&self) -> Result<String> {
        if let Some(path) = &self.file {
            if path.as_os_str() == "-" {
                return read_stdin();
            }
            return std::fs::read_to_string(path)
                .with_context(|| format!("could not read {}", path.display()));
        }
        if !self.text.is_empty() {
            return Ok(self.text.join(" "));
        }
        if std::io::stdin().is_terminal() {
            bail!("no text given (pass it as an argument, with --file, or pipe it in)");
        }
        read_stdin()
    }
}

fn read_stdin() -> Result<String> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("could not read standard input")?;
    Ok(text)
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn load_settings(path: &std::path::Path) -> Settings {
    Settings::load(path).unwrap_or_else(|e| {
        eprintln!("warning: {e:#}; using default settings");
        Settings::default()
    })
}

fn dispatch(cli: Cli) -> Result<()> {
    let settings_path = cli.config.unwrap_or_else(Settings::default_path);
    let Some(command) = cli.command else {
        // Plain start: one panel is enough.
        if ipc::send(&Request::Show)? {
            return Ok(());
        }
        return gui::run(settings_path, None);
    };

    // Requests that start the panel if it is not running yet.
    let starting = |request: Request| -> Result<()> {
        if ipc::send(&request)? {
            Ok(())
        } else {
            gui::run(settings_path.clone(), Some(request))
        }
    };
    // Requests that only make sense while it is running.
    let running = |request: Request| -> Result<()> {
        if ipc::send(&request)? {
            Ok(())
        } else {
            bail!("Better Basic TTS is not running")
        }
    };

    match command {
        Command::Speak(args) => {
            let text = args.source.read()?;
            let headless = args.output.is_some() || args.no_gui;
            if !headless && args.voice.is_none() && args.rate.is_none() && args.engine.is_none() {
                return starting(Request::Speak(text));
            }
            let mut settings = load_settings(&settings_path);
            if let Some(engine) = args.engine {
                settings.engine = engine;
            }
            if let Some(voice) = args.voice {
                settings.voice = voice;
            }
            if let Some(rate) = args.rate {
                settings.rate = rate;
            }
            settings.normalize();
            match args.output {
                Some(path) => headless::render_to_file(&settings, &text, &path),
                None => headless::speak_and_wait(&settings, &text),
            }
        }
        Command::SpeakSelection => starting(Request::SpeakSelection),
        Command::SpeakClipboard => starting(Request::SpeakClipboard),
        Command::Show => starting(Request::Show),
        Command::Pause => running(Request::PauseResume),
        Command::Stop => running(Request::Stop),
        Command::Previous => running(Request::Previous),
        Command::Next => running(Request::Next),
        Command::Slower => running(Request::Slower),
        Command::Faster => running(Request::Faster),
        Command::Voices { engine } => {
            let mut settings = load_settings(&settings_path);
            if let Some(engine) = engine {
                settings.engine = engine;
            }
            headless::list_voices(&settings)
        }
        Command::Engines => headless::list_engines(&load_settings(&settings_path)),
        Command::Explain(source) => {
            let settings = load_settings(&settings_path);
            headless::explain(&settings, &source.read()?);
            Ok(())
        }
    }
}
