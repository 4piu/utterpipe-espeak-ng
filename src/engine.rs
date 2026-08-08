#![allow(unsafe_code)]

use std::{
    collections::HashSet,
    ffi::{CStr, CString, c_int, c_void},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    DEFAULT_VOICE_ID,
    audio::{WavMetadata, pcm16_mono_wav},
    bundle::{BundleError, ensure_bundled_data},
    config::{ProviderOptions, UtteranceOptions},
    ffi,
};

const MAX_VOICES: usize = 4_096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Voice {
    pub id: String,
    pub name: String,
    pub language: String,
    pub gender: String,
}

#[derive(Clone)]
pub struct Engine {
    bundle_root: PathBuf,
    version: String,
    voices: Vec<Voice>,
}

pub struct SynthesizedAudio {
    pub bytes: Vec<u8>,
    pub metadata: WavMetadata,
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error("the bundled eSpeak NG engine could not be initialized")]
    Initialize,
    #[error("the bundled eSpeak NG catalog is invalid")]
    InvalidCatalog,
    #[error("the selected eSpeak NG voice is unavailable")]
    VoiceMissing,
    #[error("synthesis text contains a NUL character")]
    InvalidText,
    #[error("an engine path cannot be represented for eSpeak NG")]
    InvalidPath,
    #[error("the bundled eSpeak NG worker could not be located")]
    WorkerMissing,
    #[error("the bundled eSpeak NG worker could not be started")]
    Start,
    #[error("the bundled eSpeak NG worker did not accept synthesis input")]
    Input,
    #[error("the bundled eSpeak NG worker output could not be read")]
    Output,
    #[error("eSpeak NG exceeded the operation deadline")]
    Timeout,
    #[error("eSpeak NG synthesis was cancelled")]
    Cancelled,
    #[error("eSpeak NG audio exceeded the negotiated byte limit")]
    OutputTooLarge,
    #[error("eSpeak NG synthesis failed")]
    Failed,
    #[error(transparent)]
    InvalidAudio(#[from] crate::audio::WavError),
    #[error(transparent)]
    InvalidOptions(#[from] crate::config::ConfigError),
}

impl Engine {
    /// Materialize embedded data and inspect the bundled native engine.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when options, cache preparation, native engine
    /// initialization, or catalog inspection fails.
    pub fn initialize(cache_dir: &Path, options: &ProviderOptions) -> Result<Self, EngineError> {
        options.validate()?;
        let bundle_root = ensure_bundled_data(cache_dir)?;
        let session = NativeSession::initialize(&bundle_root)?;
        let version = session.version()?;
        let voices = session.voices()?;
        Ok(Self {
            bundle_root,
            version,
            voices,
        })
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub fn voices(&self) -> &[Voice] {
        &self.voices
    }

    #[must_use]
    pub fn has_voice(&self, voice_id: &str) -> bool {
        self.voices.iter().any(|voice| voice.id == voice_id)
    }

    /// Synthesize one utterance in a cancellable, bounded worker process.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] for invalid selection/text, worker failures,
    /// cancellation, timeout, overflow, or invalid output audio.
    pub fn synthesize(
        &self,
        voice_id: &str,
        text: &str,
        options: &UtteranceOptions,
        max_audio_bytes: usize,
        timeout: Duration,
        cancellation: &CancellationToken,
    ) -> Result<SynthesizedAudio, EngineError> {
        if !self.has_voice(voice_id) {
            return Err(EngineError::VoiceMissing);
        }
        if text.contains('\0') {
            return Err(EngineError::InvalidText);
        }
        let executable = std::env::current_exe().map_err(|_| EngineError::WorkerMissing)?;
        if !executable.is_file() {
            return Err(EngineError::WorkerMissing);
        }
        let mut arguments = vec![
            "engine-worker".to_owned(),
            "--bundle-root".to_owned(),
            self.bundle_root.to_string_lossy().into_owned(),
            "--voice".to_owned(),
            voice_id.to_owned(),
            "--max-audio-bytes".to_owned(),
            max_audio_bytes.to_string(),
        ];
        if let Some(value) = options.rate_wpm {
            arguments.extend(["--rate-wpm".to_owned(), value.to_string()]);
        }
        if let Some(value) = options.pitch {
            arguments.extend(["--pitch".to_owned(), value.to_string()]);
        }
        if let Some(value) = options.amplitude {
            arguments.extend(["--amplitude".to_owned(), value.to_string()]);
        }
        let bytes = run_bounded(
            &executable,
            &arguments,
            text.as_bytes(),
            max_audio_bytes,
            timeout,
            cancellation,
        )?;
        let metadata = crate::audio::inspect_pcm16_wav(&bytes)?;
        Ok(SynthesizedAudio { bytes, metadata })
    }
}

/// Execute the private synchronous native-engine worker.
///
/// # Errors
///
/// Returns [`EngineError`] for invalid controls, native initialization or
/// synthesis failure, invalid text, or output exceeding `max_audio_bytes`.
pub fn run_worker(
    bundle_root: &Path,
    voice_id: &str,
    options: &UtteranceOptions,
    text: &str,
    max_audio_bytes: usize,
) -> Result<Vec<u8>, EngineError> {
    options.validate()?;
    if text.contains('\0') {
        return Err(EngineError::InvalidText);
    }
    let mut session = NativeSession::initialize(bundle_root)?;
    session.select_voice(voice_id)?;
    session.set_options(options)?;
    let maximum_samples = max_audio_bytes.saturating_sub(44) / 2;
    let (samples, sample_rate_hz) = session.synthesize(text, maximum_samples)?;
    pcm16_mono_wav(&samples, sample_rate_hz, max_audio_bytes)
        .map_err(|_| EngineError::OutputTooLarge)
}

struct NativeSession {
    sample_rate_hz: u32,
}

#[allow(clippy::unused_self)] // `self` is the RAII guard for eSpeak's global session.
impl NativeSession {
    fn initialize(bundle_root: &Path) -> Result<Self, EngineError> {
        let path = path_cstring(bundle_root)?;
        // SAFETY: `path` is NUL-terminated and lives through the call. The linked
        // library is the pinned eSpeak NG implementation matching this declaration.
        let sample_rate =
            unsafe { ffi::espeak_Initialize(ffi::AUDIO_OUTPUT_SYNCHRONOUS, 100, path.as_ptr(), 0) };
        if sample_rate <= 0 {
            return Err(EngineError::Initialize);
        }
        Ok(Self {
            sample_rate_hz: u32::try_from(sample_rate).map_err(|_| EngineError::Initialize)?,
        })
    }

    fn version(&self) -> Result<String, EngineError> {
        // SAFETY: the initialized library owns this static NUL-terminated string.
        let pointer = unsafe { ffi::espeak_Info(std::ptr::null_mut()) };
        c_string(pointer, 256).ok_or(EngineError::InvalidCatalog)
    }

    fn voices(&self) -> Result<Vec<Voice>, EngineError> {
        let mut voices = vec![Voice {
            id: DEFAULT_VOICE_ID.to_owned(),
            name: "eSpeak NG default".to_owned(),
            language: "en".to_owned(),
            gender: "unspecified".to_owned(),
        }];
        let mut seen = HashSet::from([DEFAULT_VOICE_ID.to_owned()]);
        // SAFETY: eSpeak NG returns a NULL-terminated array valid until termination.
        let list = unsafe { ffi::espeak_ListVoices(std::ptr::null_mut()) };
        if list.is_null() {
            return Err(EngineError::InvalidCatalog);
        }
        for index in 0..MAX_VOICES {
            // SAFETY: the API contract guarantees a NULL-terminated pointer array.
            let voice = unsafe { *list.add(index) };
            if voice.is_null() {
                break;
            }
            // SAFETY: entries in the array point to initialized Voice structures.
            let voice = unsafe { &*voice };
            let Some(id) = c_string(voice.identifier, 256) else {
                continue;
            };
            // Upstream uses the platform path separator in voice identifiers.
            // Keep the public protocol catalog stable across operating systems.
            let id = id.replace('\\', "/");
            let Some(name) = c_string(voice.name, 256) else {
                continue;
            };
            let Some(language) = primary_language(voice.languages) else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            let gender = match voice.gender {
                1 => "male",
                2 => "female",
                _ => "unspecified",
            };
            voices.push(Voice {
                id,
                name,
                language,
                gender: gender.to_owned(),
            });
        }
        if voices.len() == 1 || voices.len() > MAX_VOICES {
            return Err(EngineError::InvalidCatalog);
        }
        voices[1..].sort_by(|left, right| {
            left.language
                .cmp(&right.language)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(voices)
    }

    fn select_voice(&mut self, voice_id: &str) -> Result<(), EngineError> {
        let selected = if voice_id == DEFAULT_VOICE_ID {
            "en"
        } else {
            voice_id
        };
        #[cfg(windows)]
        let selected = selected.replace('/', "\\");
        let selected = CString::new(selected).map_err(|_| EngineError::VoiceMissing)?;
        // SAFETY: selected is a live NUL-terminated string and eSpeak copies/uses it
        // synchronously when selecting the voice.
        let result = unsafe { ffi::espeak_SetVoiceByName(selected.as_ptr()) };
        if result == ffi::OK {
            Ok(())
        } else {
            Err(EngineError::VoiceMissing)
        }
    }

    fn set_options(&mut self, options: &UtteranceOptions) -> Result<(), EngineError> {
        for (parameter, value) in [
            (ffi::PARAMETER_RATE, options.rate_wpm.map(i32::from)),
            (ffi::PARAMETER_PITCH, options.pitch.map(i32::from)),
            (ffi::PARAMETER_VOLUME, options.amplitude.map(i32::from)),
        ] {
            if let Some(value) = value {
                // SAFETY: the session is initialized and parameter/value pairs are
                // validated against the documented eSpeak ranges.
                if unsafe { ffi::espeak_SetParameter(parameter, value, 0) } != ffi::OK {
                    return Err(EngineError::Failed);
                }
            }
        }
        Ok(())
    }

    fn synthesize(
        &mut self,
        text: &str,
        maximum_samples: usize,
    ) -> Result<(Vec<i16>, u32), EngineError> {
        let text = CString::new(text).map_err(|_| EngineError::InvalidText)?;
        let mut capture = Capture {
            samples: Vec::new(),
            maximum_samples,
            overflowed: false,
        };
        // SAFETY: callback has the exact C ABI expected by the pinned library.
        unsafe { ffi::espeak_SetSynthCallback(Some(capture_callback)) };
        // SAFETY: all pointers remain live for this synchronous synthesis call;
        // user_data points to `capture`, which the callback alone mutates.
        let result = unsafe {
            ffi::espeak_Synth(
                text.as_ptr().cast::<c_void>(),
                text.as_bytes_with_nul().len(),
                0,
                ffi::POSITION_CHARACTER,
                0,
                ffi::CHARS_UTF8 | ffi::END_PAUSE,
                std::ptr::null_mut(),
                std::ptr::from_mut(&mut capture).cast::<c_void>(),
            )
        };
        if capture.overflowed {
            return Err(EngineError::OutputTooLarge);
        }
        if result != ffi::OK || capture.samples.is_empty() {
            return Err(EngineError::Failed);
        }
        Ok((capture.samples, self.sample_rate_hz))
    }
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        // SAFETY: each worker/probe owns one initialized process-global session.
        let _ = unsafe { ffi::espeak_Terminate() };
    }
}

struct Capture {
    samples: Vec<i16>,
    maximum_samples: usize,
    overflowed: bool,
}

unsafe extern "C" fn capture_callback(
    samples: *mut i16,
    count: c_int,
    events: *mut ffi::Event,
) -> c_int {
    if count < 0 || events.is_null() {
        return 1;
    }
    // SAFETY: eSpeak supplies at least the terminating event, whose user_data is
    // the pointer passed to espeak_Synth. It remains live for this callback.
    let capture = unsafe { (*events).user_data.cast::<Capture>().as_mut() };
    let Some(capture) = capture else {
        return 1;
    };
    let Ok(count) = usize::try_from(count) else {
        return 1;
    };
    if count == 0 {
        return 0;
    }
    if samples.is_null()
        || capture
            .samples
            .len()
            .checked_add(count)
            .is_none_or(|total| total > capture.maximum_samples)
    {
        capture.overflowed = true;
        return 1;
    }
    // SAFETY: eSpeak guarantees `count` initialized samples when the pointer is
    // non-NULL. The slice is copied before the callback returns.
    let generated = unsafe { std::slice::from_raw_parts(samples, count) };
    capture.samples.extend_from_slice(generated);
    0
}

fn path_cstring(path: &Path) -> Result<CString, EngineError> {
    let path = path.to_str().ok_or(EngineError::InvalidPath)?;
    CString::new(path).map_err(|_| EngineError::InvalidPath)
}

fn c_string(pointer: *const i8, maximum: usize) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: callers use pointers documented by eSpeak as NUL-terminated strings.
    let value = unsafe { CStr::from_ptr(pointer) }.to_str().ok()?;
    let length = value.chars().count();
    ((1..=maximum).contains(&length) && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn primary_language(languages: *const i8) -> Option<String> {
    if languages.is_null() {
        return None;
    }
    // SAFETY: eSpeak documents this as a priority byte followed by a
    // NUL-terminated language string.
    let language = unsafe { languages.cast::<u8>().add(1).cast::<i8>() };
    c_string(language, 128)
}

fn run_bounded(
    executable: &Path,
    arguments: &[String],
    input: &[u8],
    maximum_output_bytes: usize,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, EngineError> {
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| EngineError::Start)?;
    let stdout = child.stdout.take().ok_or_else(|| {
        terminate(&mut child);
        EngineError::Output
    })?;
    let overflowed = Arc::new(AtomicBool::new(false));
    let reader_overflowed = overflowed.clone();
    let reader = thread::Builder::new()
        .name("utterpipe-espeak-output".into())
        .spawn(move || read_bounded(stdout, maximum_output_bytes, &reader_overflowed))
        .map_err(|_| {
            terminate(&mut child);
            EngineError::Output
        })?;

    let write_result = child
        .stdin
        .take()
        .ok_or(EngineError::Input)
        .and_then(|mut stdin| stdin.write_all(input).map_err(|_| EngineError::Input));
    if let Err(error) = write_result {
        terminate(&mut child);
        let _ = reader.join();
        return Err(error);
    }

    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(EngineError::Timeout)?;
    let status = loop {
        if cancellation.is_cancelled() {
            terminate(&mut child);
            let _ = reader.join();
            return Err(EngineError::Cancelled);
        }
        if overflowed.load(Ordering::Acquire) {
            terminate(&mut child);
            let _ = reader.join();
            return Err(EngineError::OutputTooLarge);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                terminate(&mut child);
                let _ = reader.join();
                return Err(EngineError::Timeout);
            }
            Err(_) => {
                terminate(&mut child);
                let _ = reader.join();
                return Err(EngineError::Failed);
            }
        }
    };
    let output = reader
        .join()
        .map_err(|_| EngineError::Output)?
        .map_err(|_| EngineError::Output)?;
    if output.len() > maximum_output_bytes {
        return Err(EngineError::OutputTooLarge);
    }
    if !status.success() {
        return Err(EngineError::Failed);
    }
    Ok(output)
}

fn read_bounded(
    mut source: impl Read,
    maximum_bytes: usize,
    overflowed: &AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            return Ok(output);
        }
        let remaining = maximum_bytes.saturating_add(1).saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
        if output.len() > maximum_bytes {
            overflowed.store(true, Ordering::Release);
        }
    }
}

fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_engine_lists_voices_and_synthesizes() {
        let temporary = tempfile::TempDir::new().unwrap();
        let engine = Engine::initialize(temporary.path(), &ProviderOptions::default()).unwrap();
        assert!(engine.version().starts_with("1.53"));
        assert!(engine.has_voice(DEFAULT_VOICE_ID));
        assert!(engine.has_voice("gmw/en"));
        let output = run_worker(
            &engine.bundle_root,
            DEFAULT_VOICE_ID,
            &UtteranceOptions::default(),
            "Hello from the bundled engine.",
            4 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(&output[..4], b"RIFF");
    }
}
