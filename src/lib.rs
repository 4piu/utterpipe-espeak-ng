pub mod audio;
pub mod bundle;
pub mod config;
pub mod engine;
mod ffi;
pub mod protocol;
pub mod wire;

pub const PROVIDER_SLUG: &str = "espeak-ng";
pub const PROVIDER_NAME: &str = "eSpeak NG TTS provider";
pub const PROVIDER_VENDOR: &str = "UtterPipe contributors";
pub const PROVIDER_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MODEL_ID: &str = "espeak-ng";
pub const DEFAULT_VOICE_ID: &str = "default";
pub const WAV_FORMAT: &str = "audio/wav;codec=pcm_s16le";
