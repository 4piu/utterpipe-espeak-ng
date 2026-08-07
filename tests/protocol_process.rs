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
                "versions": [1],
                "expected_provider": "espeak-ng",
                "session": session,
                "host": {"name": "integration-test", "version": "0.1.0"}
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
                "options": {
                    "rate_wpm": 180,
                    "pitch": 40,
                    "amplitude": 120
                },
                "selection": {"model_id": "espeak-ng", "voice_id": "default"},
                "limits": {
                    "max_text_code_points": 4096,
                    "max_audio_bytes": 1_048_576,
                    "synthesis_timeout_ms": 5000
                },
                "accepted_delivery_modes": ["complete"],
                "accepted_audio_formats": ["audio/wav;codec=pcm_s16le"]
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
    assert_eq!(hello["result"]["delivery_modes"], json!(["complete"]));
    assert_eq!(
        hello["result"]["audio_formats"],
        json!(["audio/wav;codec=pcm_s16le"])
    );
    let initialized = provider.initialize(&temp);
    assert_eq!(initialized["result"]["delivery_mode"], "complete");

    provider.request("synth", "synthesis.start", json!({"text": "Hello"}));
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
fn management_catalogs_report_embedded_engine_and_voices() {
    let temp = TempDir::new().unwrap();
    let mut provider = ProviderProcess::spawn();
    let hello = provider.hello("management");
    assert_eq!(hello["result"]["capabilities"]["model_catalog"], true);
    assert_eq!(hello["result"]["capabilities"]["voice_catalog"], true);
    provider.initialize(&temp);

    provider.request(
        "models",
        "catalog.models",
        json!({"scope": "all", "refresh": false}),
    );
    let models = provider.response();
    assert_eq!(models["result"]["models"][0]["id"], "espeak-ng");
    assert_eq!(models["result"]["models"][0]["status"], "embedded");

    provider.request(
        "voices",
        "catalog.voices",
        json!({"model_id": "espeak-ng", "scope": "all", "refresh": false}),
    );
    let voices = provider.response();
    assert_eq!(voices["result"]["voices"][0]["id"], "default");
    assert!(
        voices["result"]["voices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|voice| voice["id"] == "gmw/en")
    );
    assert_eq!(voices["result"]["voices"][0]["kind"], "embedded");
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
                provider.initialize_paths(&data_dir, &cache_dir)["result"]["delivery_mode"],
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
