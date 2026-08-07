use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    MODEL_ID, PROVIDER_NAME, PROVIDER_SLUG, PROVIDER_VENDOR, PROVIDER_VERSION, WAV_FORMAT,
    config::{ProviderOptions, options_schema},
    engine::{Engine, EngineError, SynthesizedAudio},
    wire::{Frame, FrameKind, Request, read_frame, write_frame},
};

const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_TEXT_CODE_POINTS: usize = 4_096;
const MAX_AUDIO_BYTES: u32 = 268_435_456;
const CANCELLATION_GRACE: Duration = Duration::from_secs(1);
const LICENSE_ID: &str = "gpl-3.0-or-later";
const LICENSE_URL: &str =
    "https://github.com/espeak-ng/espeak-ng/blob/359f5f397b85baf875089d3af9cda946bef31dcb/COPYING";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum SessionMode {
    Inspect,
    Runtime,
    Management,
}

#[derive(Debug, Error)]
pub enum ProtocolFailure {
    #[error("protocol input failed")]
    Input,
    #[error("protocol output failed")]
    Output,
    #[error("synthesis task failed")]
    Task,
}

struct InitializedState {
    engine: Engine,
    voice_id: String,
    max_text_code_points: usize,
    max_audio_bytes: usize,
    synthesis_timeout: Duration,
}

struct ActiveSynthesis {
    id: String,
    cancellation: CancellationToken,
    task: JoinHandle<Result<SynthesizedAudio, EngineError>>,
}

enum LoopEvent {
    Input(Result<Option<Frame>, crate::wire::WireFailure>),
    Synthesis(Result<Result<SynthesizedAudio, EngineError>, tokio::task::JoinError>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloParams {
    protocol: String,
    versions: Vec<u64>,
    expected_provider: String,
    session: SessionMode,
    host: HostIdentity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostIdentity {
    name: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializeParams {
    data_dir: String,
    cache_dir: String,
    options: Map<String, Value>,
    selection: Selection,
    limits: RuntimeLimits,
    accepted_delivery_modes: Vec<String>,
    accepted_audio_formats: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    model_id: String,
    voice_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeLimits {
    max_text_code_points: u64,
    max_audio_bytes: u64,
    synthesis_timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SynthesisParams {
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelParams {
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogParams {
    scope: String,
    refresh: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VoiceCatalogParams {
    model_id: String,
    scope: String,
    refresh: bool,
}

#[derive(Debug)]
struct WireError {
    code: &'static str,
    message: String,
}

impl WireError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Serve one `UtterPipe` protocol session on standard input/output.
///
/// # Errors
///
/// Returns [`ProtocolFailure`] after malformed framing/input, failed protocol
/// output, or an unexpected synthesis-task failure.
#[allow(clippy::too_many_lines)]
pub async fn run_stdio() -> Result<(), ProtocolFailure> {
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut session = None;
    let mut initialized = None;
    let mut active: Option<ActiveSynthesis> = None;

    loop {
        let event = if let Some(active_synthesis) = active.as_mut() {
            tokio::select! {
                biased;
                input = read_frame(&mut stdin) => LoopEvent::Input(input),
                outcome = &mut active_synthesis.task => LoopEvent::Synthesis(outcome),
            }
        } else {
            LoopEvent::Input(read_frame(&mut stdin).await)
        };

        match event {
            LoopEvent::Synthesis(outcome) => {
                let completed = active.take().ok_or(ProtocolFailure::Task)?;
                write_synthesis_outcome(&mut stdout, &completed.id, outcome).await?;
            }
            LoopEvent::Input(Ok(None)) => {
                stop_active(active.take()).await;
                return Ok(());
            }
            LoopEvent::Input(Err(_)) => {
                stop_active(active.take()).await;
                return Err(ProtocolFailure::Input);
            }
            LoopEvent::Input(Ok(Some(frame))) => {
                if frame.kind != FrameKind::Control {
                    stop_active(active.take()).await;
                    return Err(ProtocolFailure::Input);
                }
                let request = Request::parse(&frame.payload).map_err(|_| ProtocolFailure::Input)?;

                if request.method == "protocol.hello" {
                    if session.is_some() {
                        write_error(
                            &mut stdout,
                            &request.id,
                            &WireError::new(
                                "invalid_state",
                                "protocol hello was already completed",
                            ),
                        )
                        .await?;
                        continue;
                    }
                    match handle_hello(&request.params) {
                        Ok((chosen, result)) => {
                            session = Some(chosen);
                            write_result(&mut stdout, &request.id, result).await?;
                        }
                        Err(error) => write_error(&mut stdout, &request.id, &error).await?,
                    }
                    continue;
                }

                let Some(chosen_session) = session else {
                    write_error(
                        &mut stdout,
                        &request.id,
                        &WireError::new("invalid_state", "protocol hello is required first"),
                    )
                    .await?;
                    continue;
                };

                if request.method == "session.shutdown" {
                    if let Err(error) = require_empty_params(&request.params) {
                        write_error(&mut stdout, &request.id, &error).await?;
                        continue;
                    }
                    stop_active(active.take()).await;
                    write_result(&mut stdout, &request.id, json!({"accepted": true})).await?;
                    return Ok(());
                }

                if request.method == "session.initialize" {
                    if chosen_session == SessionMode::Inspect {
                        write_error(
                            &mut stdout,
                            &request.id,
                            &WireError::new(
                                "wrong_session",
                                "inspect sessions cannot be initialized",
                            ),
                        )
                        .await?;
                        continue;
                    }
                    if initialized.is_some() {
                        write_error(
                            &mut stdout,
                            &request.id,
                            &WireError::new("invalid_state", "session is already initialized"),
                        )
                        .await?;
                        continue;
                    }
                    match initialize(&request.params).await {
                        Ok(state) => {
                            initialized = Some(state);
                            write_result(
                                &mut stdout,
                                &request.id,
                                json!({
                                    "ready": true,
                                    "delivery_mode": "complete",
                                    "audio_format": WAV_FORMAT,
                                    "options_schema_version": 1
                                }),
                            )
                            .await?;
                        }
                        Err(error) => write_error(&mut stdout, &request.id, &error).await?,
                    }
                    continue;
                }

                if chosen_session == SessionMode::Inspect {
                    let code = if is_runtime_method(&request.method)
                        || is_management_method(&request.method)
                    {
                        "wrong_session"
                    } else {
                        "method_not_supported"
                    };
                    write_error(
                        &mut stdout,
                        &request.id,
                        &WireError::new(code, "method is unavailable in an inspect session"),
                    )
                    .await?;
                    continue;
                }

                let Some(state) = initialized.as_ref() else {
                    write_error(
                        &mut stdout,
                        &request.id,
                        &WireError::new("invalid_state", "session is not initialized"),
                    )
                    .await?;
                    continue;
                };

                match (chosen_session, request.method.as_str()) {
                    (SessionMode::Runtime, "runtime.health") => {
                        if let Err(error) = require_empty_params(&request.params) {
                            write_error(&mut stdout, &request.id, &error).await?;
                        } else {
                            write_result(&mut stdout, &request.id, json!({"status": "ready"}))
                                .await?;
                        }
                    }
                    (SessionMode::Runtime, "synthesis.start") if active.is_some() => {
                        write_error(
                            &mut stdout,
                            &request.id,
                            &WireError::new("busy", "another synthesis request is active"),
                        )
                        .await?;
                    }
                    (SessionMode::Runtime, "synthesis.start") => {
                        let params: SynthesisParams = match decode_params(&request.params) {
                            Ok(params) => params,
                            Err(error) => {
                                write_error(&mut stdout, &request.id, &error).await?;
                                continue;
                            }
                        };
                        let length = params.text.chars().count();
                        if length == 0
                            || length > state.max_text_code_points
                            || params.text.contains('\0')
                        {
                            write_error(
                                &mut stdout,
                                &request.id,
                                &WireError::new(
                                    "invalid_text",
                                    "text is empty or exceeds the negotiated code-point limit",
                                ),
                            )
                            .await?;
                            continue;
                        }
                        let engine = state.engine.clone();
                        let voice_id = state.voice_id.clone();
                        let maximum = state.max_audio_bytes;
                        let timeout = state.synthesis_timeout;
                        let cancellation = CancellationToken::new();
                        let task_cancellation = cancellation.clone();
                        let task = tokio::task::spawn_blocking(move || {
                            engine.synthesize(
                                &voice_id,
                                &params.text,
                                maximum,
                                timeout,
                                &task_cancellation,
                            )
                        });
                        active = Some(ActiveSynthesis {
                            id: request.id,
                            cancellation,
                            task,
                        });
                    }
                    (SessionMode::Runtime, "synthesis.cancel") => {
                        let params: CancelParams = match decode_params(&request.params) {
                            Ok(params) => params,
                            Err(error) => {
                                write_error(&mut stdout, &request.id, &error).await?;
                                continue;
                            }
                        };
                        let accepted = active
                            .as_ref()
                            .is_some_and(|current| current.id == params.request_id);
                        if accepted && let Some(current) = active.as_ref() {
                            current.cancellation.cancel();
                        }
                        write_result(&mut stdout, &request.id, json!({"accepted": accepted}))
                            .await?;
                    }
                    (SessionMode::Management, "provider.validate") => {
                        if let Err(error) = require_empty_params(&request.params) {
                            write_error(&mut stdout, &request.id, &error).await?;
                        } else {
                            write_result(
                                &mut stdout,
                                &request.id,
                                json!({"status": "ready", "issues": []}),
                            )
                            .await?;
                        }
                    }
                    (SessionMode::Management, "catalog.models") => {
                        match catalog_models(&request.params, state) {
                            Ok(result) => write_result(&mut stdout, &request.id, result).await?,
                            Err(error) => write_error(&mut stdout, &request.id, &error).await?,
                        }
                    }
                    (SessionMode::Management, "catalog.voices") => {
                        match catalog_voices(&request.params, state) {
                            Ok(result) => write_result(&mut stdout, &request.id, result).await?,
                            Err(error) => write_error(&mut stdout, &request.id, &error).await?,
                        }
                    }
                    (
                        SessionMode::Management,
                        "prepare.plan" | "prepare.apply" | "remove.plan" | "remove.apply"
                        | "voice.import",
                    ) => {
                        write_error(
                            &mut stdout,
                            &request.id,
                            &WireError::new(
                                "method_not_supported",
                                "the engine and voice data are embedded in the provider",
                            ),
                        )
                        .await?;
                    }
                    (SessionMode::Runtime, method) if is_management_method(method) => {
                        write_error(
                            &mut stdout,
                            &request.id,
                            &WireError::new(
                                "wrong_session",
                                "management method is unavailable in a runtime session",
                            ),
                        )
                        .await?;
                    }
                    (SessionMode::Management, method) if is_runtime_method(method) => {
                        write_error(
                            &mut stdout,
                            &request.id,
                            &WireError::new(
                                "wrong_session",
                                "runtime method is unavailable in a management session",
                            ),
                        )
                        .await?;
                    }
                    _ => {
                        write_error(
                            &mut stdout,
                            &request.id,
                            &WireError::new("method_not_supported", "unknown protocol method"),
                        )
                        .await?;
                    }
                }
            }
        }
    }
}

fn handle_hello(params: &Map<String, Value>) -> Result<(SessionMode, Value), WireError> {
    let hello: HelloParams = decode_params(params)?;
    if hello
        .versions
        .iter()
        .any(|version| *version > MAX_SAFE_JSON_INTEGER)
    {
        return Err(WireError::new(
            "invalid_message",
            "protocol versions contain an out-of-range integer",
        ));
    }
    if hello.protocol != "utterpipe.tts" || !hello.versions.contains(&1) {
        return Err(WireError::new(
            "unsupported_protocol",
            "the host did not offer utterpipe.tts protocol major 1",
        ));
    }
    if !valid_text(&hello.expected_provider, 64)
        || !valid_text(&hello.host.name, 256)
        || !valid_text(&hello.host.version, 256)
    {
        return Err(WireError::new(
            "invalid_message",
            "hello identity fields are invalid",
        ));
    }
    Ok((
        hello.session,
        json!({
            "protocol": "utterpipe.tts",
            "version": 1,
            "provider": {
                "slug": PROVIDER_SLUG,
                "name": PROVIDER_NAME,
                "vendor": PROVIDER_VENDOR,
                "version": PROVIDER_VERSION
            },
            "capabilities": {
                "synthesis": true,
                "cancellation": true,
                "model_catalog": true,
                "voice_catalog": true,
                "prepare": false,
                "remove": false,
                "voice_import": false
            },
            "delivery_modes": ["complete"],
            "audio_formats": [WAV_FORMAT],
            "options_schema": options_schema()
        }),
    ))
}

async fn initialize(params: &Map<String, Value>) -> Result<InitializedState, WireError> {
    let params: InitializeParams = decode_params(params)?;
    let data = Path::new(&params.data_dir);
    let cache = Path::new(&params.cache_dir);
    if !data.is_absolute() || !cache.is_absolute() || paths_are_equivalent(data, cache) {
        return Err(WireError::new(
            "invalid_options",
            "data_dir and cache_dir must be different absolute paths",
        ));
    }
    if params.selection.model_id != MODEL_ID {
        return Err(WireError::new(
            "invalid_selection",
            "model_id must be espeak-ng",
        ));
    }
    if !valid_text(&params.selection.voice_id, 256) {
        return Err(WireError::new("invalid_selection", "voice_id is invalid"));
    }
    if params.limits.max_text_code_points > MAX_SAFE_JSON_INTEGER
        || params.limits.max_audio_bytes > MAX_SAFE_JSON_INTEGER
        || params.limits.synthesis_timeout_ms > MAX_SAFE_JSON_INTEGER
        || params.limits.max_text_code_points == 0
        || params.limits.max_audio_bytes == 0
        || params.limits.synthesis_timeout_ms == 0
    {
        return Err(WireError::new(
            "invalid_options",
            "all negotiated limits must be positive protocol integers",
        ));
    }
    if !params
        .accepted_delivery_modes
        .iter()
        .any(|mode| mode == "complete")
        || !params
            .accepted_audio_formats
            .iter()
            .any(|format| format == WAV_FORMAT)
    {
        return Err(WireError::new(
            "invalid_options",
            "the host must accept complete PCM16 WAV delivery",
        ));
    }
    let options: ProviderOptions = serde_json::from_value(Value::Object(params.options))
        .map_err(|_| WireError::new("invalid_options", "provider options are invalid"))?;
    options
        .validate()
        .map_err(|error| WireError::new("invalid_options", error.to_string()))?;
    let cache = cache.to_path_buf();
    let engine = tokio::task::spawn_blocking(move || Engine::initialize(&cache, &options))
        .await
        .map_err(|_| WireError::new("internal", "engine discovery task failed"))?
        .map_err(map_initialization_error)?;
    if !engine.has_voice(&params.selection.voice_id) {
        return Err(WireError::new(
            "invalid_selection",
            "the selected eSpeak NG voice is unavailable",
        ));
    }
    let max_text_code_points = usize::try_from(
        params
            .limits
            .max_text_code_points
            .min(MAX_TEXT_CODE_POINTS as u64),
    )
    .map_err(|_| WireError::new("invalid_options", "text limit is unsupported"))?;
    let max_audio_bytes = usize::try_from(
        params
            .limits
            .max_audio_bytes
            .min(u64::from(MAX_AUDIO_BYTES)),
    )
    .map_err(|_| WireError::new("invalid_options", "audio limit is unsupported"))?;
    let synthesis_timeout = Duration::from_millis(params.limits.synthesis_timeout_ms);
    if std::time::Instant::now()
        .checked_add(synthesis_timeout)
        .is_none()
    {
        return Err(WireError::new(
            "invalid_options",
            "synthesis timeout is too large for this platform",
        ));
    }
    Ok(InitializedState {
        engine,
        voice_id: params.selection.voice_id,
        max_text_code_points,
        max_audio_bytes,
        synthesis_timeout,
    })
}

fn catalog_models(
    params: &Map<String, Value>,
    state: &InitializedState,
) -> Result<Value, WireError> {
    let params: CatalogParams = decode_params(params)?;
    validate_catalog_request(&params.scope, params.refresh)?;
    let mut languages = state
        .engine
        .voices()
        .iter()
        .filter(|voice| voice.id != crate::DEFAULT_VOICE_ID)
        .map(|voice| voice.language.clone())
        .collect::<Vec<_>>();
    languages.sort();
    languages.dedup();
    Ok(json!({
        "models": [{
            "id": MODEL_ID,
            "name": "Bundled eSpeak NG",
            "version": state.engine.version(),
            "status": "embedded",
            "languages": languages,
            "license": license_descriptor()
        }]
    }))
}

fn catalog_voices(
    params: &Map<String, Value>,
    state: &InitializedState,
) -> Result<Value, WireError> {
    let params: VoiceCatalogParams = decode_params(params)?;
    validate_catalog_request(&params.scope, params.refresh)?;
    if params.model_id != MODEL_ID {
        return Err(WireError::new(
            "invalid_selection",
            "voice catalog model_id must be espeak-ng",
        ));
    }
    let voices = state
        .engine
        .voices()
        .iter()
        .map(|voice| {
            json!({
                "id": voice.id,
                "name": voice.name,
                "status": "embedded",
                "kind": "embedded",
                "languages": [voice.language],
                "license": license_descriptor()
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"voices": voices}))
}

fn validate_catalog_request(scope: &str, _refresh: bool) -> Result<(), WireError> {
    if !matches!(scope, "installed" | "available" | "all") {
        return Err(WireError::new(
            "invalid_options",
            "catalog scope must be installed, available, or all",
        ));
    }
    Ok(())
}

fn license_descriptor() -> Value {
    json!({
        "id": LICENSE_ID,
        "url": LICENSE_URL,
        "requires_acceptance": false
    })
}

async fn write_synthesis_outcome<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    id: &str,
    outcome: Result<Result<SynthesizedAudio, EngineError>, tokio::task::JoinError>,
) -> Result<(), ProtocolFailure> {
    match outcome {
        Ok(Ok(audio)) => {
            write_result(
                writer,
                id,
                json!({
                    "audio": {
                        "format": WAV_FORMAT,
                        "byte_length": audio.bytes.len(),
                        "sample_rate_hz": audio.metadata.sample_rate_hz,
                        "channels": audio.metadata.channels
                    }
                }),
            )
            .await?;
            write_frame(writer, &Frame::audio(audio.bytes))
                .await
                .map_err(|_| ProtocolFailure::Output)
        }
        Ok(Err(error)) => write_error(writer, id, &map_synthesis_error(&error)).await,
        Err(_) => {
            write_error(
                writer,
                id,
                &WireError::new("internal", "synthesis task failed"),
            )
            .await
        }
    }
}

fn map_initialization_error(error: EngineError) -> WireError {
    match error {
        EngineError::InvalidOptions(error) => WireError::new("invalid_options", error.to_string()),
        EngineError::VoiceMissing => WireError::new("invalid_selection", error.to_string()),
        _ => WireError::new("engine_unavailable", error.to_string()),
    }
}

fn map_synthesis_error(error: &EngineError) -> WireError {
    match error {
        EngineError::Cancelled => WireError::new("cancelled", error.to_string()),
        EngineError::Timeout => WireError::new("timeout", error.to_string()),
        EngineError::OutputTooLarge => WireError::new("output_too_large", error.to_string()),
        EngineError::VoiceMissing => WireError::new("voice_missing", error.to_string()),
        EngineError::Initialize
        | EngineError::Bundle(_)
        | EngineError::WorkerMissing
        | EngineError::Start => WireError::new("engine_unavailable", error.to_string()),
        EngineError::InvalidText => WireError::new("invalid_text", error.to_string()),
        _ => WireError::new("synthesis_failed", error.to_string()),
    }
}

async fn stop_active(active: Option<ActiveSynthesis>) {
    let Some(mut active) = active else {
        return;
    };
    active.cancellation.cancel();
    if tokio::time::timeout(CANCELLATION_GRACE, &mut active.task)
        .await
        .is_err()
    {
        active.task.abort();
    }
}

async fn write_result<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    id: &str,
    result: Value,
) -> Result<(), ProtocolFailure> {
    let payload = serde_json::to_vec(&json!({
        "kind": "response",
        "id": id,
        "result": result
    }))
    .map_err(|_| ProtocolFailure::Output)?;
    write_frame(writer, &Frame::control(payload))
        .await
        .map_err(|_| ProtocolFailure::Output)
}

async fn write_error<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    id: &str,
    error: &WireError,
) -> Result<(), ProtocolFailure> {
    let payload = serde_json::to_vec(&json!({
        "kind": "response",
        "id": id,
        "error": {"code": error.code, "message": error.message}
    }))
    .map_err(|_| ProtocolFailure::Output)?;
    write_frame(writer, &Frame::control(payload))
        .await
        .map_err(|_| ProtocolFailure::Output)
}

fn decode_params<T: for<'de> Deserialize<'de>>(
    params: &Map<String, Value>,
) -> Result<T, WireError> {
    serde_json::from_value(Value::Object(params.clone()))
        .map_err(|_| WireError::new("invalid_message", "request parameters are invalid"))
}

fn require_empty_params(params: &Map<String, Value>) -> Result<(), WireError> {
    if params.is_empty() {
        Ok(())
    } else {
        Err(WireError::new(
            "invalid_message",
            "request parameters must be empty",
        ))
    }
}

fn valid_text(value: &str, maximum: usize) -> bool {
    let length = value.chars().count();
    (1..=maximum).contains(&length) && !value.contains(['\r', '\n', '\0'])
}

fn paths_are_equivalent(first: &Path, second: &Path) -> bool {
    if first == second || normalize_absolute_path(first) == normalize_absolute_path(second) {
        return true;
    }
    match (std::fs::canonicalize(first), std::fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => false,
    }
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn is_runtime_method(method: &str) -> bool {
    matches!(
        method,
        "runtime.health" | "synthesis.start" | "synthesis.cancel"
    )
}

fn is_management_method(method: &str) -> bool {
    matches!(
        method,
        "provider.validate"
            | "catalog.models"
            | "catalog.voices"
            | "prepare.plan"
            | "prepare.apply"
            | "remove.plan"
            | "remove.apply"
            | "voice.import"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_path_aliases_are_equivalent() {
        let root = if cfg!(windows) { r"C:\roots" } else { "/roots" };
        assert!(paths_are_equivalent(
            &Path::new(root).join("data/../cache"),
            &Path::new(root).join("cache")
        ));
    }

    #[test]
    fn hello_requires_protocol_and_returns_exact_identity() {
        let params = json!({
            "protocol": "utterpipe.tts",
            "versions": [1],
            "expected_provider": PROVIDER_SLUG,
            "session": "inspect",
            "host": {"name": "test", "version": "0.1.0"}
        });
        let (session, result) = handle_hello(params.as_object().unwrap()).unwrap();
        assert_eq!(session, SessionMode::Inspect);
        assert_eq!(result["provider"]["slug"], PROVIDER_SLUG);
        assert_eq!(result["delivery_modes"], json!(["complete"]));
        assert_eq!(result["audio_formats"], json!([WAV_FORMAT]));
    }
}
