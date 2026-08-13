# Agent Speak setup

This guide connects the eSpeak NG provider to Agent Speak and reaches an
audible MCP test. The provider synthesizes WAV audio; Agent Speak owns the audio
device, permissions, queue, and playback gain.

## 1. Make the provider discoverable

Agent Speak searches for `utterpipe-espeak-ng` beside its own executable and on
its inherited `PATH`.

When using the Agent Speak VS Code extension, run **Agent Speak: Open Provider
Folder** from the Command Palette and copy the provider executable into that
folder. This is more reliable than relying on the `PATH` inherited by a GUI
application, and it keeps the extension's provider installation isolated.
`command -v utterpipe-espeak-ng` on macOS/Linux or
`(Get-Command utterpipe-espeak-ng).Source` in PowerShell shows the installed
file to copy.

## 2. Configure Agent Speak

For a new standalone profile, copy
[`agent-speak.example.toml`](agent-speak.example.toml) to a writable location.
The file is a complete profile and enables arbitrary text only; local audio
files remain disabled.

For an existing profile, replace its current `[tts]` section and any child TTS
tables with:

```toml
[tts]
enabled = true
provider = "utterpipe-espeak-ng"
maximum_characters = 300
agent_utterance_options = ["voice", "rate_wpm", "pitch"]

[tts.utterance_options]
voice = "default"
rate_wpm = 175
pitch = 50
amplitude = 100
```

The VS Code extension exposes its isolated profile through **Agent Speak:
Settings** and the **Open config.toml** link. Provider-specific settings use the
advanced TOML route.

## 3. Validate the provider path and profile

Pass the same absolute profile path that the MCP server will use:

```sh
agent-speak validate --config /path/to/agent-speak.toml
agent-speak provider info --config /path/to/agent-speak.toml
agent-speak provider catalog --config /path/to/agent-speak.toml --catalog voices
```

The first command validates the complete policy and initializes the configured
provider read-only. The second shows the resolved provider executable and its
capabilities. The third prints the embedded voice catalog and copyable option
patches.

## 4. Hear a test

Configure the MCP client to launch:

```text
agent-speak serve --config /absolute/path/to/agent-speak.toml
```

Restart the MCP server after changing the profile. In a connected chat, invoke
`get_audio_capabilities` first; it should list `speak_text`. Then invoke
`speak_text` with text such as “Agent Speak eSpeak test successful.” Audio
should play from the profile's default output.

If `speak_text` is absent, confirm that `[permissions].arbitrary_text` and
`[tts].enabled` are both `true`. If provider discovery fails in VS Code, use
**Agent Speak: Open Provider Folder** instead of assuming the integrated
terminal and local UI extension host share a `PATH`.
