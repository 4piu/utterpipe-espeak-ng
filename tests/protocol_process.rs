use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Barrier},
    thread,
};

use serde_json::{Value, json};
use tempfile::TempDir;

const CONTROL: u8 = 0x01;
const AUDIO: u8 = 0x02;

struct ProviderProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl ProviderProcess {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_utterpipe-espeak-ng"))
            .args(["protocol", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            stdin: child.stdin.take().unwrap(),
            stdout: child.stdout.take().unwrap(),
            child,
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn request(&mut self, id: &str, method: &str, params: Value) {
        write_frame(
            &mut self.stdin,
            CONTROL,
            &serde_json::to_vec(&json!({
                "kind": "request",
                "id": id,
                "method": method,
                "params": params
            }))
            .unwrap(),
        );
    }

    fn response(&mut self) -> Value {
        let (kind, payload) = read_frame(&mut self.stdout);
        assert_eq!(kind, CONTROL);
        serde_json::from_slice(&payload).unwrap()
    }

    fn hello(&mut self, session: &str) -> Value {
        self.request(
            "hello",
            "protocol.hello",
            json!({
                "protocol": "utterpipe.tts",
                "versions": [2],
                "expected_provider": "espeak-ng",
                "session": session,
                "utterance_schema_profiles": ["utterpipe.utterance-options/1"],
                "host": {"name": "integration-test", "version": "0.2.0"}
            }),
        );
        self.response()
    }

    fn initialize(&mut self, temp: &TempDir) -> Value {
        self.initialize_paths(&temp.path().join("data"), &temp.path().join("cache"))
    }

    fn initialize_paths(&mut self, data_dir: &Path, cache_dir: &Path) -> Value {
        self.request(
            "init",
            "session.initialize",
            json!({
                "data_dir": data_dir.to_string_lossy(),
                "cache_dir": cache_dir.to_string_lossy(),
                "provider_options": {
                    "voice": "default",
                    "rate_wpm": 180,
                    "pitch": 40,
                    "amplitude": 120
                },
                "limits": {
                    "max_text_code_points": 4096,
                    "max_audio_bytes": 1_048_576,
                    "synthesis_timeout_ms": 5000
                },
                "accepted_audio_deliveries": [
                    {"mode":"complete", "format":"audio/wav;codec=pcm_s16le"}
                ]
            }),
        );
        self.response()
    }

    fn initialize_management(&mut self, temp: &TempDir) -> Value {
        self.request(
            "init",
            "session.initialize",
            json!({
                "data_dir": temp.path().join("data").to_string_lossy(),
                "cache_dir": temp.path().join("cache").to_string_lossy(),
                "provider_options": {}
            }),
        );
        self.response()
    }

    fn shutdown(mut self) {
        self.request("shutdown", "session.shutdown", json!({}));
        assert_eq!(self.response()["result"]["accepted"], true);
        drop(self.stdin);
        assert!(self.child.wait().unwrap().success());
    }
}

fn write_frame(writer: &mut impl Write, kind: u8, payload: &[u8]) {
    let mut header = [0_u8; 12];
    header[..4].copy_from_slice(b"UTP1");
    header[4] = kind;
    header[8..].copy_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
    writer.write_all(&header).unwrap();
    writer.write_all(payload).unwrap();
    writer.flush().unwrap();
}

fn read_frame(reader: &mut impl Read) -> (u8, Vec<u8>) {
    let mut header = [0_u8; 12];
    reader.read_exact(&mut header).unwrap();
    assert_eq!(&header[..4], b"UTP1");
    assert_eq!(&header[5..8], &[0, 0, 0]);
    let length = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).unwrap();
    (header[4], payload)
}

#[test]
fn complete_synthesis_normalizes_wav_and_reuses_runtime() {
    let temp = TempDir::new().unwrap();
    let mut provider = ProviderProcess::spawn();
    let hello = provider.hello("runtime");
    assert_eq!(hello["result"]["provider"]["slug"], "espeak-ng");
    assert_eq!(
        hello["result"]["audio_deliveries"],
        json!([{"mode":"complete", "format":"audio/wav;codec=pcm_s16le"}])
    );
    let initialized = provider.initialize(&temp);
    assert_eq!(initialized["result"]["audio_delivery"]["mode"], "complete");
    assert!(
        initialized["result"]["utterance_options_schema_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );

    provider.request(
        "synth",
        "synthesis.start",
        json!({"text": "Hello", "utterance_options":{"rate_wpm":220, "pitch":60}}),
    );
    let terminal = provider.response();
    assert_eq!(terminal["result"]["audio"]["sample_rate_hz"], 22_050);
    assert_eq!(terminal["result"]["audio"]["channels"], 1);
    let (kind, audio) = read_frame(&mut provider.stdout);
    assert_eq!(kind, AUDIO);
    assert_eq!(&audio[..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(audio[4..8].try_into().unwrap()) as usize + 8,
        audio.len()
    );
    assert_eq!(
        u32::from_le_bytes(audio[40..44].try_into().unwrap()) as usize,
        audio.len() - 44
    );

    provider.request("health", "runtime.health", json!({}));
    assert_eq!(provider.response()["result"]["status"], "ready");
    provider.shutdown();
}

#[test]
fn active_synthesis_is_busy_and_cancellable() {
    let temp = TempDir::new().unwrap();
    let mut provider = ProviderProcess::spawn();
    provider.hello("runtime");
    provider.initialize(&temp);
    provider.request(
        "synth",
        "synthesis.start",
        json!({"text": "A deliberately long cancellation test. ".repeat(100)}),
    );
    provider.request("second", "synthesis.start", json!({"text": "second"}));
    assert_eq!(provider.response()["error"]["code"], "busy");
    provider.request("cancel", "synthesis.cancel", json!({"request_id": "synth"}));
    assert_eq!(provider.response()["result"]["accepted"], true);
    assert_eq!(provider.response()["error"]["code"], "cancelled");
    provider.shutdown();
}

#[test]
fn invalid_utterance_options_do_not_start_or_poison_synthesis() {
    let temp = TempDir::new().unwrap();
    let mut provider = ProviderProcess::spawn();
    provider.hello("runtime");
    provider.initialize(&temp);
    provider.request(
        "invalid",
        "synthesis.start",
        json!({"text":"do not synthesize", "utterance_options":{"rate_wpm":451}}),
    );
    assert_eq!(
        provider.response()["error"]["code"],
        "invalid_utterance_options"
    );
    provider.request("health", "runtime.health", json!({}));
    assert_eq!(provider.response()["result"]["status"], "ready");
    provider.shutdown();
}

#[test]
fn management_catalogs_report_embedded_engine_and_voices() {
    let temp = TempDir::new().unwrap();
    let mut provider = ProviderProcess::spawn();
    let hello = provider.hello("management");
    assert!(
        hello["result"]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("catalog"))
    );
    provider.initialize_management(&temp);
    provider.request("validate", "provider.validate", json!({}));
    assert_eq!(provider.response()["result"]["status"], "ready");

    provider.request(
        "models",
        "catalog.items",
        json!({"catalog_id":"models", "scope": "all", "refresh": false, "limit":100}),
    );
    let models = provider.response();
    assert_eq!(models["result"]["items"][0]["id"], "espeak-ng");
    assert_eq!(models["result"]["items"][0]["status"], "embedded");

    provider.request(
        "voices",
        "catalog.items",
        json!({"catalog_id":"voices", "scope": "all", "refresh": false, "limit":1}),
    );
    let voices = provider.response();
    assert_eq!(voices["result"]["items"][0]["id"], "default");
    assert_eq!(
        voices["result"]["items"][0]["provider_options_patch"]["voice"],
        "default"
    );
    let cursor = voices["result"]["next_cursor"].as_str().unwrap();
    provider.request(
        "voices-next",
        "catalog.items",
        json!({
            "catalog_id":"voices", "scope":"all", "refresh":false,
            "limit":256, "cursor":cursor
        }),
    );
    let voices = provider.response();
    assert!(
        voices["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|voice| voice["id"] == "gmw/en")
    );
    provider.shutdown();
}

#[test]
fn parallel_provider_instances_share_one_cache() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    let cache_dir = temp.path().join("cache");
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();

    for index in 0..2 {
        let data_dir = data_dir.clone();
        let cache_dir = cache_dir.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            let mut provider = ProviderProcess::spawn();
            assert_eq!(
                provider.hello("runtime")["result"]["provider"]["slug"],
                "espeak-ng"
            );
            barrier.wait();
            assert_eq!(
                provider.initialize_paths(&data_dir, &cache_dir)["result"]["audio_delivery"]
                    ["mode"],
                "complete"
            );
            provider.request(
                "synth",
                "synthesis.start",
                json!({"text": format!("Parallel provider {index}")}),
            );
            assert_eq!(
                provider.response()["result"]["audio"]["sample_rate_hz"],
                22_050
            );
            let (kind, audio) = read_frame(&mut provider.stdout);
            assert_eq!(kind, AUDIO);
            assert_eq!(&audio[..4], b"RIFF");
            provider.shutdown();
        }));
    }

    for worker in workers {
        worker.join().unwrap();
    }
    assert!(cache_dir.join("espeak-ng-1.53.0-data-v1").is_dir());
}
