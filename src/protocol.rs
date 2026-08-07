use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    MODEL_ID, PROVIDER_NAME, PROVIDER_SLUG, PROVIDER_VENDOR, PROVIDER_VERSION, WAV_FORMAT,
    config::{
        ProviderOptions, UtteranceOptions, management_options_schema, provider_options_schema,
        utterance_options_schema,
    },
    engine::{Engine, EngineError, SynthesizedAudio},
    wire::{Frame, FrameKind, Request, read_frame, write_frame},
};

const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;
const UTTERANCE_SCHEMA_PROFILE: &str = "utterpipe.utterance-options/1";
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
    provider_options: ProviderOptions,
    max_text_code_points: usize,
    max_audio_bytes: usize,
    synthesis_timeout: Duration,
}

struct ActiveSynthesis {
    id: String,
    cancellation: CancellationToken,
    cancellation_accepted: bool,
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
    utterance_schema_profiles: Vec<String>,
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
struct RuntimeInitializeParams {
    data_dir: String,
    cache_dir: String,
    provider_options: Map<String, Value>,
    limits: RuntimeLimits,
    accepted_audio_deliveries: Vec<AudioDelivery>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagementInitializeParams {
    data_dir: String,
    cache_dir: String,
    provider_options: Map<String, Value>,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct AudioDelivery {
    mode: String,
    format: String,
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
    #[serde(default)]
    utterance_options: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelParams {
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogItemsParams {
    catalog_id: String,
    scope: String,
    refresh: bool,
    limit: u16,
    #[serde(default)]
    cursor: Option<String>,
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
                if completed.cancellation_accepted {
                    write_error(
                        &mut stdout,
                        &completed.id,
                        &WireError::new("cancelled", "synthesis was cancelled"),
                    )
                    .await?;
                } else {
                    write_synthesis_outcome(&mut stdout, &completed.id, outcome).await?;
                }
            }
            LoopEvent::Input(Ok(None)) => {
                stop_active(active.take()).await?;
                return Ok(());
            }
            LoopEvent::Input(Err(_)) => {
                stop_active(active.take()).await?;
                return Err(ProtocolFailure::Input);
            }
            LoopEvent::Input(Ok(Some(frame))) => {
                if frame.kind != FrameKind::Control {
                    stop_active(active.take()).await?;
                    return Err(ProtocolFailure::Input);
                }
                let request = Request::parse(&frame.payload).map_err(|_| ProtocolFailure::Input)?;

                if active
                    .as_ref()
                    .is_some_and(|current| current.id == request.id)
                {
                    stop_active(active.take()).await?;
                    return Err(ProtocolFailure::Input);
                }

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
                    if let Some(current) = active.take() {
                        let synthesis_id = current.id.clone();
                        stop_active(Some(current)).await?;
                        write_error(
                            &mut stdout,
                            &synthesis_id,
                            &WireError::new("cancelled", "synthesis was cancelled by shutdown"),
                        )
                        .await?;
                    }
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
                    match chosen_session {
                        SessionMode::Runtime => match initialize_runtime(&request.params).await {
                            Ok(state) => {
                                let schema = utterance_options_schema();
                                let digest = utterance_schema_digest(&schema)
                                    .map_err(|_| ProtocolFailure::Task)?;
                                initialized = Some(state);
                                write_result(
                                    &mut stdout,
                                    &request.id,
                                    json!({
                                        "ready": true,
                                        "audio_delivery": {
                                            "mode": "complete",
                                            "format": WAV_FORMAT
                                        },
                                        "utterance_options_schema": schema,
                                        "utterance_options_schema_digest": digest
                                    }),
                                )
                                .await?;
                            }
                            Err(error) => write_error(&mut stdout, &request.id, &error).await?,
                        },
                        SessionMode::Management => {
                            match initialize_management(&request.params).await {
                                Ok(state) => {
                                    initialized = Some(state);
                                    write_result(&mut stdout, &request.id, json!({"ready": true}))
                                        .await?;
                                }
                                Err(error) => {
                                    write_error(&mut stdout, &request.id, &error).await?;
                                }
                            }
                        }
                        SessionMode::Inspect => {
                            unreachable!("inspect initialization rejected above")
                        }
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
                        let Ok(utterance_options): Result<UtteranceOptions, _> =
                            serde_json::from_value(Value::Object(params.utterance_options.clone()))
                        else {
                            write_error(
                                &mut stdout,
                                &request.id,
                                &WireError::new(
                                    "invalid_utterance_options",
                                    "utterance options do not match the resolved schema",
                                ),
                            )
                            .await?;
                            continue;
                        };
                        if utterance_options.validate().is_err() {
                            write_error(
                                &mut stdout,
                                &request.id,
                                &WireError::new(
                                    "invalid_utterance_options",
                                    "utterance options do not match the resolved schema",
                                ),
                            )
                            .await?;
                            continue;
                        }
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
                        let options = state.provider_options.with_utterance(&utterance_options);
                        let maximum = state.max_audio_bytes;
                        let timeout = state.synthesis_timeout;
                        let cancellation = CancellationToken::new();
                        let task_cancellation = cancellation.clone();
                        let task = tokio::task::spawn_blocking(move || {
                            engine.synthesize(
                                &voice_id,
                                &params.text,
                                &options,
                                maximum,
                                timeout,
                                &task_cancellation,
                            )
                        });
                        active = Some(ActiveSynthesis {
                            id: request.id,
                            cancellation,
                            cancellation_accepted: false,
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
                        if accepted && let Some(current) = active.as_mut() {
                            current.cancellation.cancel();
                            current.cancellation_accepted = true;
                        }
                        write_result(&mut stdout, &request.id, json!({"accepted": accepted}))
                            .await?;
                    }
                    (SessionMode::Management, "provider.validate") => {
                        if let Err(error) = require_empty_params(&request.params) {
                            write_error(&mut stdout, &request.id, &error).await?;
                        } else {
                            let (status, issues) = if state.engine.has_voice(&state.voice_id) {
                                ("ready", Vec::new())
                            } else {
                                (
                                    "unavailable",
                                    vec![json!({
                                        "severity":"error",
                                        "code":"voice_unavailable",
                                        "message":"The configured eSpeak NG voice is unavailable.",
                                        "remediation":"Choose a voice from the voices catalog or omit the voice option."
                                    })],
                                )
                            };
                            write_result(
                                &mut stdout,
                                &request.id,
                                json!({"status": status, "issues": issues}),
                            )
                            .await?;
                        }
                    }
                    (SessionMode::Management, "catalog.items") => {
                        match catalog_items(&request.params, state) {
                            Ok(result) => write_result(&mut stdout, &request.id, result).await?,
                            Err(error) => write_error(&mut stdout, &request.id, &error).await?,
                        }
                    }
                    (
                        SessionMode::Management,
                        "prepare.plan" | "prepare.apply" | "remove.plan" | "remove.apply"
                        | "asset.import",
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
    if hello.expected_provider != PROVIDER_SLUG {
        return Err(WireError::new(
            "provider_mismatch",
            "expected provider does not match this provider",
        ));
    }
    if !hello
        .utterance_schema_profiles
        .iter()
        .any(|profile| profile == UTTERANCE_SCHEMA_PROFILE)
    {
        return Err(WireError::new(
            "unsupported_schema_profile",
            "the host did not offer utterpipe.utterance-options/1",
        ));
    }
    if !valid_text(&hello.host.name, 256) || !valid_text(&hello.host.version, 256) {
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
            "framing": "UTP1",
            "provider": {
                "slug": PROVIDER_SLUG,
                "name": PROVIDER_NAME,
                "vendor": PROVIDER_VENDOR,
                "version": PROVIDER_VERSION
            },
            "capabilities": ["synthesis", "synthesis.cancel", "catalog"],
            "audio_deliveries": [{"mode":"complete", "format":WAV_FORMAT}],
            "utterance_schema_profile": UTTERANCE_SCHEMA_PROFILE,
            "provider_options_schema": provider_options_schema(),
            "management_options_schema": management_options_schema(),
            "catalogs": [
                {
                    "id":"models",
                    "name":"Models",
                    "description":"The embedded eSpeak NG synthesis engine.",
                    "item_kind":"model",
                    "patchable_options":[]
                },
                {
                    "id":"voices",
                    "name":"Voices",
                    "description":"Voices embedded with eSpeak NG.",
                    "item_kind":"voice",
                    "patchable_options":["voice"]
                }
            ],
            "import_kinds": []
        }),
    ))
}

async fn initialize_runtime(params: &Map<String, Value>) -> Result<InitializedState, WireError> {
    let params: RuntimeInitializeParams = decode_params(params)?;
    validate_roots(&params.data_dir, &params.cache_dir)?;
    if params.limits.max_text_code_points > MAX_SAFE_JSON_INTEGER
        || params.limits.max_audio_bytes > MAX_SAFE_JSON_INTEGER
        || params.limits.synthesis_timeout_ms > MAX_SAFE_JSON_INTEGER
        || params.limits.max_audio_bytes > u64::from(u32::MAX)
        || params.limits.max_text_code_points == 0
        || params.limits.max_audio_bytes == 0
        || params.limits.synthesis_timeout_ms == 0
    {
        return Err(WireError::new(
            "invalid_message",
            "all negotiated limits must be positive protocol integers",
        ));
    }
    if params.accepted_audio_deliveries.len() != 1
        || params.accepted_audio_deliveries[0].mode != "complete"
        || params.accepted_audio_deliveries[0].format != WAV_FORMAT
    {
        return Err(WireError::new(
            "invalid_message",
            "the host must offer the advertised complete PCM16 WAV delivery",
        ));
    }
    let options = decode_provider_options(params.provider_options)?;
    let engine = initialize_engine(&params.cache_dir, &options).await?;
    let voice_id = options.resolved_voice().to_owned();
    if !engine.has_voice(&voice_id) {
        return Err(WireError::new(
            "invalid_provider_options",
            "the configured eSpeak NG voice is unavailable",
        ));
    }
    let max_text_code_points = usize::try_from(
        params
            .limits
            .max_text_code_points
            .min(MAX_TEXT_CODE_POINTS as u64),
    )
    .map_err(|_| WireError::new("invalid_message", "text limit is unsupported"))?;
    let max_audio_bytes = usize::try_from(
        params
            .limits
            .max_audio_bytes
            .min(u64::from(MAX_AUDIO_BYTES)),
    )
    .map_err(|_| WireError::new("invalid_message", "audio limit is unsupported"))?;
    let synthesis_timeout = Duration::from_millis(params.limits.synthesis_timeout_ms);
    if std::time::Instant::now()
        .checked_add(synthesis_timeout)
        .is_none()
    {
        return Err(WireError::new(
            "invalid_message",
            "synthesis timeout is too large for this platform",
        ));
    }
    Ok(InitializedState {
        engine,
        voice_id,
        provider_options: options,
        max_text_code_points,
        max_audio_bytes,
        synthesis_timeout,
    })
}

async fn initialize_management(params: &Map<String, Value>) -> Result<InitializedState, WireError> {
    let params: ManagementInitializeParams = decode_params(params)?;
    validate_roots(&params.data_dir, &params.cache_dir)?;
    let options = decode_provider_options(params.provider_options)?;
    let engine = initialize_engine(&params.cache_dir, &options).await?;
    Ok(InitializedState {
        voice_id: options.resolved_voice().to_owned(),
        engine,
        provider_options: options,
        max_text_code_points: 0,
        max_audio_bytes: 0,
        synthesis_timeout: Duration::ZERO,
    })
}

fn decode_provider_options(options: Map<String, Value>) -> Result<ProviderOptions, WireError> {
    let options: ProviderOptions = serde_json::from_value(Value::Object(options))
        .map_err(|_| WireError::new("invalid_provider_options", "provider options are invalid"))?;
    options
        .validate()
        .map_err(|error| WireError::new("invalid_provider_options", error.to_string()))?;
    Ok(options)
}

async fn initialize_engine(
    cache_dir: &str,
    options: &ProviderOptions,
) -> Result<Engine, WireError> {
    let cache = Path::new(cache_dir).to_path_buf();
    let options = options.clone();
    tokio::task::spawn_blocking(move || Engine::initialize(&cache, &options))
        .await
        .map_err(|_| WireError::new("internal", "engine discovery task failed"))?
        .map_err(map_initialization_error)
}

fn validate_roots(data_dir: &str, cache_dir: &str) -> Result<(), WireError> {
    let data = Path::new(data_dir);
    let cache = Path::new(cache_dir);
    if !data.is_absolute() || !cache.is_absolute() || paths_overlap(data, cache) {
        Err(WireError::new(
            "invalid_message",
            "data_dir and cache_dir must be distinct, non-nested absolute paths",
        ))
    } else {
        Ok(())
    }
}

fn catalog_items(
    params: &Map<String, Value>,
    state: &InitializedState,
) -> Result<Value, WireError> {
    if params.get("cursor").is_some_and(Value::is_null) {
        return Err(WireError::new(
            "invalid_message",
            "catalog cursor is invalid",
        ));
    }
    let params: CatalogItemsParams = decode_params(params)?;
    validate_catalog_request(&params.scope, params.refresh)?;
    if !(1..=256).contains(&params.limit) {
        return Err(WireError::new(
            "invalid_message",
            "catalog limit must be from 1 through 256",
        ));
    }
    let items = match params.catalog_id.as_str() {
        "models" => model_items(state),
        "voices" => voice_items(state),
        _ => return Err(WireError::new("invalid_message", "catalog ID is unknown")),
    };
    let offset = parse_catalog_cursor(params.cursor.as_deref(), items.len())?;
    let end = (offset + usize::from(params.limit)).min(items.len());
    let page = items[offset..end].to_vec();
    let next_cursor = (end < items.len()).then(|| format!("offset:{end}"));
    let mut result = json!({"items": page});
    if let Some(next_cursor) = next_cursor {
        result["next_cursor"] = Value::String(next_cursor);
    }
    Ok(result)
}

fn validate_catalog_request(scope: &str, _refresh: bool) -> Result<(), WireError> {
    if !matches!(scope, "installed" | "available" | "all") {
        return Err(WireError::new(
            "invalid_message",
            "catalog scope must be installed, available, or all",
        ));
    }
    Ok(())
}

fn parse_catalog_cursor(cursor: Option<&str>, length: usize) -> Result<usize, WireError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let offset = cursor
        .strip_prefix("offset:")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|offset| *offset <= length)
        .ok_or_else(|| WireError::new("invalid_message", "catalog cursor is invalid"))?;
    Ok(offset)
}

fn model_items(state: &InitializedState) -> Vec<Value> {
    let mut languages = state
        .engine
        .voices()
        .iter()
        .filter(|voice| voice.id != crate::DEFAULT_VOICE_ID)
        .map(|voice| voice.language.clone())
        .collect::<Vec<_>>();
    languages.sort();
    languages.dedup();
    vec![json!({
        "id": MODEL_ID,
        "name": "Bundled eSpeak NG",
        "description": format!("Embedded eSpeak NG {} synthesis engine.", state.engine.version()),
        "status": "embedded",
        "languages": languages,
        "provider_options_patch": {},
        "artifacts": [],
        "license": license_descriptor()
    })]
}

fn voice_items(state: &InitializedState) -> Vec<Value> {
    state
        .engine
        .voices()
        .iter()
        .map(|voice| {
            json!({
                "id": voice.id,
                "name": voice.name,
                "description": format!("Embedded {} eSpeak NG voice.", voice.gender),
                "status": "embedded",
                "languages": [voice.language],
                "provider_options_patch": {"voice": voice.id},
                "artifacts": [],
                "license": license_descriptor()
            })
        })
        .collect()
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
        EngineError::InvalidOptions(error) => {
            WireError::new("invalid_provider_options", error.to_string())
        }
        EngineError::VoiceMissing => WireError::new("invalid_provider_options", error.to_string()),
        _ => WireError::new("engine_unavailable", error.to_string()),
    }
}

fn map_synthesis_error(error: &EngineError) -> WireError {
    match error {
        EngineError::Cancelled => WireError::new("cancelled", error.to_string()),
        EngineError::Timeout => WireError::new("timeout", error.to_string()),
        EngineError::OutputTooLarge => WireError::new("output_too_large", error.to_string()),
        EngineError::VoiceMissing => WireError::new("resource_missing", error.to_string()),
        EngineError::Initialize
        | EngineError::Bundle(_)
        | EngineError::WorkerMissing
        | EngineError::Start => WireError::new("engine_unavailable", error.to_string()),
        EngineError::InvalidText => WireError::new("invalid_text", error.to_string()),
        _ => WireError::new("synthesis_failed", error.to_string()),
    }
}

async fn stop_active(active: Option<ActiveSynthesis>) -> Result<(), ProtocolFailure> {
    let Some(mut active) = active else {
        return Ok(());
    };
    active.cancellation.cancel();
    if tokio::time::timeout(CANCELLATION_GRACE, &mut active.task)
        .await
        .is_err()
    {
        active.task.abort();
        return Err(ProtocolFailure::Task);
    }
    Ok(())
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

fn paths_overlap(first: &Path, second: &Path) -> bool {
    let first = std::fs::canonicalize(first).unwrap_or_else(|_| normalize_absolute_path(first));
    let second = std::fs::canonicalize(second).unwrap_or_else(|_| normalize_absolute_path(second));
    first == second || first.starts_with(&second) || second.starts_with(&first)
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
            | "catalog.items"
            | "prepare.plan"
            | "prepare.apply"
            | "remove.plan"
            | "remove.apply"
            | "asset.import"
    )
}

fn utterance_schema_digest(schema: &Value) -> Result<String, serde_json::Error> {
    let canonical = serde_json_canonicalizer::to_vec(schema)?;
    let digest = Sha256::digest(canonical);
    Ok(format!("sha256:{digest:x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_path_aliases_and_nested_roots_overlap() {
        let root = if cfg!(windows) { r"C:\roots" } else { "/roots" };
        assert!(paths_overlap(
            &Path::new(root).join("data/../cache"),
            &Path::new(root).join("cache")
        ));
        assert!(paths_overlap(
            &Path::new(root).join("data"),
            &Path::new(root).join("data/cache")
        ));
    }

    #[test]
    fn hello_requires_protocol_and_returns_exact_identity() {
        let params = json!({
            "protocol": "utterpipe.tts",
            "versions": [1],
            "expected_provider": PROVIDER_SLUG,
            "session": "inspect",
            "utterance_schema_profiles": [UTTERANCE_SCHEMA_PROFILE],
            "host": {"name": "test", "version": "0.1.0"}
        });
        let (session, result) = handle_hello(params.as_object().unwrap()).unwrap();
        assert_eq!(session, SessionMode::Inspect);
        assert_eq!(result["provider"]["slug"], PROVIDER_SLUG);
        assert_eq!(
            result["audio_deliveries"],
            json!([{"mode":"complete", "format":WAV_FORMAT}])
        );
    }
}
