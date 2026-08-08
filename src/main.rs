use std::{
    io::{Read, Write},
    path::PathBuf,
};

use clap::{Parser, Subcommand};
use utterpipe_espeak_ng::{
    MODEL_ID, PROVIDER_NAME, PROVIDER_SLUG, PROVIDER_VENDOR, PROVIDER_VERSION,
    config::{ProviderOptions, UtteranceOptions},
    engine::{Engine, run_worker},
    protocol,
};

const MAX_WORKER_TEXT_BYTES: u64 = 64 * 1024;

#[derive(Debug, Parser)]
#[command(name = "utterpipe-espeak-ng", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print provider identity and capabilities.
    Info,
    /// Verify the bundled engine and voice catalog.
    Doctor {
        /// Cache directory used for reconstructible bundled engine data.
        #[arg(long)]
        cache_dir: Option<PathBuf>,
    },
    /// List voices exposed by the bundled engine.
    Voices {
        /// Cache directory used for reconstructible bundled engine data.
        #[arg(long)]
        cache_dir: Option<PathBuf>,
    },
    /// Run the framed `UtterPipe` protocol.
    Protocol {
        #[arg(long, required = true)]
        stdio: bool,
    },
    #[command(hide = true)]
    EngineWorker {
        #[arg(long)]
        bundle_root: PathBuf,
        #[arg(long)]
        voice: String,
        #[arg(long)]
        max_audio_bytes: usize,
        #[arg(long)]
        rate_wpm: Option<u16>,
        #[arg(long)]
        pitch: Option<u8>,
        #[arg(long)]
        amplitude: Option<u8>,
    },
}

#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("error: {message}");
            1
        }
    };
    if code != 0 {
        std::process::exit(code);
    }
}

async fn run() -> Result<(), String> {
    match Cli::parse().command {
        Command::Info => {
            println!("{PROVIDER_NAME}");
            println!("slug: {PROVIDER_SLUG}");
            println!("vendor: {PROVIDER_VENDOR}");
            println!("version: {PROVIDER_VERSION}");
            println!("protocol: utterpipe.tts v1");
            println!("model: {MODEL_ID}");
            println!("engine: bundled eSpeak NG 1.53.0 (359f5f3)");
            println!("delivery: complete PCM16 WAV");
            println!("capabilities: synthesis, cancellation, generic catalogs");
            Ok(())
        }
        Command::Doctor { cache_dir } => {
            let engine =
                Engine::initialize(&resolve_cache_dir(cache_dir), &ProviderOptions::default())
                    .map_err(|error| error.to_string())?;
            println!("ready: eSpeak NG {}", engine.version());
            println!("voices: {}", engine.voices().len());
            Ok(())
        }
        Command::Voices { cache_dir } => {
            let engine =
                Engine::initialize(&resolve_cache_dir(cache_dir), &ProviderOptions::default())
                    .map_err(|error| error.to_string())?;
            for voice in engine.voices() {
                println!("{}\t{}\t{}", voice.id, voice.language, voice.name);
            }
            Ok(())
        }
        Command::Protocol { stdio } => {
            debug_assert!(stdio);
            protocol::run_stdio()
                .await
                .map_err(|error| error.to_string())
        }
        Command::EngineWorker {
            bundle_root,
            voice,
            max_audio_bytes,
            rate_wpm,
            pitch,
            amplitude,
        } => {
            let mut text = String::new();
            std::io::stdin()
                .take(MAX_WORKER_TEXT_BYTES + 1)
                .read_to_string(&mut text)
                .map_err(|_| "worker input could not be read".to_owned())?;
            if text.len() as u64 > MAX_WORKER_TEXT_BYTES {
                return Err("worker input is too large".to_owned());
            }
            let options = UtteranceOptions {
                voice: None,
                rate_wpm,
                pitch,
                amplitude,
            };
            let audio = run_worker(&bundle_root, &voice, &options, &text, max_audio_bytes)
                .map_err(|error| error.to_string())?;
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(&audio)
                .and_then(|()| stdout.flush())
                .map_err(|_| "worker output could not be written".to_owned())
        }
    }
}

fn resolve_cache_dir(configured: Option<PathBuf>) -> PathBuf {
    configured.unwrap_or_else(|| std::env::temp_dir().join("utterpipe-espeak-ng"))
}
