# UtterPipe eSpeak NG provider

`utterpipe-espeak-ng` is an offline text-to-speech provider for UtterPipe hosts
such as [Agent Speak](https://github.com/4piu/agent-speak). It produces complete
22.05 kHz mono PCM16 WAV audio; the host handles playback and device routing.

**One executable contains the eSpeak NG engine, 148 standard voices, languages,
dictionaries, and phoneme data.** You do not need to install eSpeak NG, Python,
a model, or a separate voice pack. MBROLA voices are not included.

## Install and verify

macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-espeak-ng/main/install.sh | sh
# Open a new terminal if the installer says the directory is not on PATH.
utterpipe-espeak-ng doctor
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/4piu/utterpipe-espeak-ng/main/install.ps1 | iex
# Open a new PowerShell window after the installer adds its directory to PATH.
utterpipe-espeak-ng doctor
```

A successful check reports the bundled eSpeak NG version and voice count. If
the shell cannot find the executable, follow the installer's instruction to add
its directory to `PATH`, then open a new shell.

The installer downloads the release for the current platform and verifies its
published SHA-256 checksum. See [installation and maintenance](docs/installation.md)
for manual verification, updates, uninstalling, supported platforms, and
building from source.

## Agent Speak configuration

Agent Speak must be able to find `utterpipe-espeak-ng` beside its own executable
or on its `PATH`. Then select this backend in a complete Agent Speak profile:

```toml
[tts]
enabled = true
backend = "utterpipe-espeak-ng"
maximum_characters = 300
agent_utterance_options = ["voice", "rate_wpm", "pitch"]

[tts.utterance_options]
voice = "default"
rate_wpm = 175
pitch = 50
amplitude = 100
```

This is a replacement for the profile's existing `[tts]` section, not a
complete profile by itself. A ready-to-copy complete profile and VS Code steps
are in [the Agent Speak setup guide](docs/agent-speak.md).

Verify the integration before starting the MCP server:

```sh
agent-speak validate --config /path/to/agent-speak.toml
agent-speak provider info --config /path/to/agent-speak.toml
```

Start or restart Agent Speak in your MCP client, call `speak_text`, and confirm
that audio plays. The provider itself does not open an audio device, so
`utterpipe-espeak-ng doctor` verifies synthesis resources but does not speak.

## Voice and speech controls

`voice` defaults to `default`. Run `utterpipe-espeak-ng voices` or
`agent-speak provider catalog --config /path/to/agent-speak.toml --catalog voices`
to list stable IDs such as `gmw/en-US`. Per-request controls are `voice`,
`rate_wpm` (80–450), `pitch` (0–100), and `amplitude` (0–200). Agent Speak
exposes only controls named in `agent_utterance_options`.

For the provider protocol, runtime storage behavior, engine scope, and exact
option semantics, see [the provider specification](docs/SPEC.md).

## License and provenance

This provider is GPL-3.0-or-later because it incorporates eSpeak NG. Binary
distributions must include [`LICENSE`](LICENSE),
[source and data provenance](THIRD_PARTY.md), and the generated
[Rust dependency notices](THIRD_PARTY_LICENSES.html). See the
[release-integrity status](docs/release-integrity.md) for current signing and
provenance details.
