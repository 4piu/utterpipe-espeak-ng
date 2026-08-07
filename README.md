# UtterPipe eSpeak NG provider

`utterpipe-espeak-ng` is a self-contained, cross-platform UtterPipe TTS
provider. It statically links the official eSpeak NG engine and embeds its
language, voice, phoneme, and dictionary data. Users download one executable;
they do not install `espeak-ng`, a language runtime, a model, or a voice pack.

The provider emits complete 22.05 kHz mono PCM16 RIFF/WAVE audio. It exposes
the upstream voice catalog and supports fixed per-session rate, pitch, and
amplitude options.

## Install

Download the archive and matching `.sha256` file for your platform from
[GitHub Releases](https://github.com/4piu/utterpipe-espeak-ng/releases), verify
the checksum, and place `utterpipe-espeak-ng` (`.exe` on Windows) beside the
host application or on `PATH`. The executable contains the engine and its
standard data; no system eSpeak package is needed. The archive also carries
the GPL license and source/provenance notices required for redistribution.

The repository also provides checksum-verifying per-user installation:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-espeak-ng/main/install.sh | sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-espeak-ng/main/install.sh | sh -s -- --uninstall
```

```powershell
irm https://raw.githubusercontent.com/4piu/utterpipe-espeak-ng/main/install.ps1 | iex
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/4piu/utterpipe-espeak-ng/main/install.ps1))) -Uninstall
```

Add `--purge` or `-Purge` only when uninstalling to irreversibly remove the
provider's reconstructible cache.

## Build

Rust 1.88+, CMake, and a C/C++ compiler are required to build from source.
Initialize the pinned upstream source when cloning:

```sh
git clone --recurse-submodules https://github.com/4piu/utterpipe-espeak-ng.git
cd utterpipe-espeak-ng
cargo build --release --locked
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

The release executable is `target/release/utterpipe-espeak-ng` (or `.exe` on
Windows). Runtime use requires no compiler, CMake, shared eSpeak library, or
system eSpeak data.

## Commands

```text
utterpipe-espeak-ng info
utterpipe-espeak-ng doctor [--cache-dir PATH]
utterpipe-espeak-ng voices [--cache-dir PATH]
utterpipe-espeak-ng protocol --stdio
```

`protocol --stdio` is the machine entry point. Standard output is reserved for
UtterPipe frames; diagnostics use standard error.

On first initialization, the provider atomically materializes its embedded
read-only engine data below the host-provided `cache_dir`. This is a bounded,
reconstructible cache, not configuration or a downloaded voice pack. Provider
instances share it through an interprocess lock. The direct diagnostic commands
default to a directory below the operating system's temporary directory when
`--cache-dir` is omitted.

## Agent Speak configuration

Place the provider executable beside `agent-speak` or on `PATH`, then select it
in `config.toml`:

```toml
[tts]
enabled = true
backend = "utterpipe"
provider = "espeak-ng"
model_id = "espeak-ng"
voice_id = "default"
maximum_characters = 300
provider_environment = []

[tts.provider_options]
rate_wpm = 175
pitch = 50
amplitude = 100
```

`rate_wpm` accepts 80–450, `pitch` 0–100, and `amplitude` 0–200. Options are
fixed for the provider session. Run `agent-speak voices --config
config.toml` to inspect stable voice IDs such as `gmw/en`.

The reusable provider process owns the UtterPipe session. Each synthesis is
performed by a short-lived worker invocation of the same executable, with text
sent over standard input. This preserves hard cancellation, timeouts, bounded
output, and process isolation without exposing spoken text in command-line
arguments or temporary files.

## Scope

- Engine: official eSpeak NG source pinned at commit `359f5f3` (1.53.0).
- Embedded compiled data: eSpeak NG 1.52.0 plus the 1.52.0.1 language data set.
- Included: standard eSpeak and Klatt/speechPlayer synthesis.
- Excluded: MBROLA voices, external sound assets, libpcaudio playback, and
  libsonic. Agent Speak remains responsible for playback and device routing.
- Delivery: complete `audio/wav;codec=pcm_s16le`; no fake incremental mode.
- Storage: `data_dir` is unused; only the reconstructible cache is written.

See [the provider specification](docs/SPEC.md) for the normative behavior and
[THIRD_PARTY.md](THIRD_PARTY.md) for source and data provenance.

This provider is GPL-3.0-or-later because it incorporates eSpeak NG. Keeping it
in a separate executable preserves license and release independence for
UtterPipe hosts. The complete GPL text is available in
[`vendor/espeak-ng/COPYING`](vendor/espeak-ng/COPYING) and must accompany every
binary distribution.
