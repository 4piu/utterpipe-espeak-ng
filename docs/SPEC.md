# Bundled eSpeak NG provider specification

Status: locally implemented; pre-release compatibility provisional

Provider slug: `espeak-ng`

Executable: `utterpipe-espeak-ng`

Initial provider version: `0.1.0`

UtterPipe protocol majors: `1`

This document is normative for this provider. It supplements the host-neutral
[UtterPipe Protocol v1](https://github.com/4piu/utterpipe/blob/main/docs/SPEC.md)
specification.

## 1. Purpose and identity

The provider makes the official eSpeak NG engine available as one downloadable
UtterPipe executable on Windows, macOS, and Linux. It does not discover or use a
system `espeak-ng` installation.

Hello reports slug `espeak-ng`, name `eSpeak NG TTS provider`, vendor
`UtterPipe contributors`, and the provider package version. The one model ID is
`espeak-ng`.

## 2. Engine and build contract

The provider statically links official eSpeak NG source pinned by the repository
submodule at commit `359f5f397b85baf875089d3af9cda946bef31dcb`
(1.53.0). Release builds MUST initialize submodules and use the committed
Cargo lockfile.

Compiled phoneme, language, voice, MBROLA-mapping, and dictionary files are
embedded from the GPL-3.0-or-later crates `espeak-ng-data-phonemes` 0.1.0,
`espeak-ng-data-dicts` 0.1.0, and `espeak-ng-data-dict-ru` 0.1.0. Those files
identify as eSpeak NG 1.52.0 plus additional 1.52.0.1 language definitions.

The build disables eSpeak's audio-device output, asynchronous queue, MBROLA
runtime, and libsonic. It keeps standard eSpeak synthesis, Klatt, and
speechPlayer. Therefore the release has no runtime eSpeak, MBROLA, audio
library, C/C++ runtime package, or model prerequisite beyond ordinary platform
system libraries.

## 3. Capabilities and audio

Hello declares:

```json
{
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
  "audio_formats": ["audio/wav;codec=pcm_s16le"]
}
```

Successful synthesis returns a finite RIFF/WAVE file containing signed 16-bit
little-endian, 22,050 Hz, mono PCM. The provider MUST validate the final
container and MUST NOT claim incremental delivery: eSpeak callback chunks are
an internal implementation detail and the host receives one complete frame.

The provider accepts at most 4,096 input Unicode scalar values and the lower of
its 256 MiB ceiling and the host's `max_audio_bytes`. Empty text and text
containing NUL are `invalid_text`.

## 4. Options

The complete v1 option set is:

| Option | Type | Default | Range | Meaning |
| --- | --- | --- | --- | --- |
| `rate_wpm` | integer | engine default 175 | 80–450 | speaking rate in words per minute |
| `pitch` | integer | engine default 50 | 0–100 | base pitch |
| `amplitude` | integer | engine default 100 | 0–200 | output volume/amplitude |

Unknown fields and out-of-range values are `invalid_options`. Options are fixed
at initialization and applied to every synthesis. The output sample rate is not
configurable in v1.

## 5. Selection and catalogs

`model_id` MUST be `espeak-ng`. `voice_id = "default"` selects upstream English
(`en`). Other accepted IDs are the stable upstream identifiers returned by
`espeak_ListVoices`, for example `gmw/en`.

`catalog.models` returns one `embedded` model with the engine version reported
by `espeak_Info`, the distinct catalog languages, and the eSpeak NG GPL license.
`catalog.voices` returns `embedded` voices with upstream name, primary language,
and stable identifier. Catalog operations are local and network-free.

Catalog scope `installed`, `available`, or `all` is accepted because embedded
assets satisfy all three views. `refresh` does not contact the network or alter
the embedded catalog.

## 6. Storage and concurrency

The provider never writes `data_dir`. On first initialization it MUST
materialize embedded data under this versioned path:

```text
<cache_dir>/espeak-ng-1.53.0-data-v1/espeak-ng-data/
```

This is the declared unavoidable bounded engine cache allowed by UtterPipe v1.
It is reconstructible from the executable and contains no configuration,
download, license acceptance, input text, or generated audio.

Extraction MUST hold `<cache_dir>/.utterpipe-espeak-ng.lock`, write to a unique
temporary sibling, validate required files and a version marker, and atomically
rename the complete tree. A failed extraction cleans its own temporary tree.
The lock is released on process exit. Once published, the versioned data tree is
read-only from the provider's perspective.

Any number of provider instances may share the same cache and embedded assets.
Each instance serves one host session and one active synthesis at a time; it is
not a daemon or cross-client multiplexer.

## 7. Runtime and cancellation

The main provider process lives for the UtterPipe session and handles hello,
initialization, health, catalogs, synthesis requests, cancellation, and clean
shutdown. It rejects a second active synthesis with `busy`.

For each synthesis it starts the same executable's private worker command.
Provider-owned options, voice ID, cache path, and output limit are arguments;
plain synthesis text is sent through worker standard input and is never a
command argument or file. Worker standard output contains only the complete WAV
and worker standard error is not relayed into protocol framing.

The parent reads worker output through a bounded reader. Cancellation, timeout,
output overflow, host EOF, and session shutdown terminate and reap that worker.
This process boundary is required because synchronous eSpeak synthesis cannot
otherwise provide a portable hard-cancellation guarantee.

## 8. Management, privacy, and network policy

The embedded engine needs no preparation, removal, import, download, license
acceptance, network access, credential, or service process. Optional management
methods return `method_not_supported` and explain that assets are embedded.
`provider.validate` and `runtime.health` are local and read-only after cache
materialization.

The provider performs no network I/O. It MUST NOT persist or log synthesis text
or audio. Process listings may expose the non-secret voice/options/cache path,
but never text.

## 9. Licensing and releases

The provider and its embedded engine/data are GPL-3.0-or-later. Model and voice
catalog descriptors use license ID `gpl-3.0-or-later`, the pinned upstream
license URL, and `requires_acceptance: false`.

Every binary distribution MUST include the GPL text, provider source location,
pinned eSpeak NG source revision, data-crate source locations and versions,
third-party notices, checksum, and target identity. A release archive SHOULD
also include an SBOM. Release source MUST be sufficient to rebuild the shipped
binary, including the initialized upstream submodule.

Initial target intent is Windows x86_64 MSVC, macOS arm64/x86_64, and Linux
x86_64 GNU. A target is supported only after its native CI and protocol tests
pass; signing/notarization status MUST be disclosed per artifact.
