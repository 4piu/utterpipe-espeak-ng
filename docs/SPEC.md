# Bundled eSpeak NG provider specification

- Status: implemented
- Provider slug: `espeak-ng`
- Executable: `utterpipe-espeak-ng`
- Provider version: `0.1.0`
- UtterPipe protocol major: `1`

This document is normative for this provider. It supplements the host-neutral
[UtterPipe v1 specification](https://github.com/4piu/utterpipe/blob/main/docs/SPEC.md).

## 1. Purpose and engine

The provider distributes the official eSpeak NG engine and standard data as one
self-contained executable for Windows, macOS, and Linux. It never discovers or
uses a system `espeak-ng` installation.

The build statically links eSpeak NG source pinned at commit
`359f5f397b85baf875089d3af9cda946bef31dcb` (1.53.0). Compiled phoneme,
language, voice, and dictionary data comes from the pinned GPL data crates
listed in `THIRD_PARTY.md`. Audio-device output, the asynchronous engine queue,
MBROLA runtime, libpcaudio, and libsonic are excluded. Standard eSpeak, Klatt,
and speechPlayer synthesis remain available.

The executable therefore has no runtime eSpeak, model, voice pack, compiler,
language runtime, or audio-library prerequisite.

## 2. Protocol identity and capabilities

Hello reports slug `espeak-ng`, name `eSpeak NG TTS provider`, vendor
`UtterPipe contributors`, and the package version. Capabilities are
`synthesis`, `synthesis.cancel`, and `catalog`.

The only audio delivery is:

```json
{"mode":"complete","format":"audio/wav;codec=pcm_s16le"}
```

Successful synthesis returns one finite RIFF/WAVE file containing signed
16-bit little-endian, 22,050 Hz, mono PCM. The provider validates the final
container. eSpeak callback chunks are internal and MUST NOT be exposed as fake
incremental delivery.

## 3. Provider and utterance options

The fixed provider schema is the closed empty object. The resolved
`utterpipe.utterance-options/1` schema exposes:

| Option | Type | Default | Range and meaning |
| --- | --- | --- | --- |
| `voice` | string | `default` | Embedded catalog voice ID, 1–256 code points |
| `rate_wpm` | integer | engine default 175 | 80–450 words per minute |
| `pitch` | integer | engine default 50 | eSpeak pitch level 0–100 |
| `amplitude` | integer | engine default 100 | eSpeak amplitude level 0–200 |

Unknown members, explicit nulls, invalid voice text, and out-of-range controls
return `invalid_utterance_options`. `default` selects upstream English; other
stable IDs come from the voices catalog, such as `gmw/en`.

All are optional. A host may send configured defaults and authorized overrides
together for one worker invocation. Omission uses the engine default, and a
request never changes later requests or persistent state. Values are rejected
before a worker starts when invalid or when a named embedded voice is absent.

## 4. Generic catalogs

Hello declares two generic catalogs:

- `models`, item kind `model`, with no patchable options;
- `voices`, item kind `voice`, with patchable utterance option `voice`.

`catalog.items` supports scopes `installed`, `available`, and `all`; embedded
assets satisfy all three. `refresh` is local and has no side effect. `limit` is
1–256 and `cursor` is an opaque provider cursor.

The model catalog returns one `embedded` item with ID `espeak-ng`, the native
engine version, distinct languages, empty option patches, and the GPL license.

Each voice item contains the stable upstream ID, name, primary language,
embedded status, license, and this patch:

```json
{"utterance_options_patch":{"voice":"<catalog item ID>"}}
```

The provider advertises no prepare, remove, or import capability. Those method
families return `method_not_supported` because engine and voices are embedded.

## 5. Initialization, cache, and concurrency

Runtime initialization requires empty fixed options, validates the exact WAV
delivery offer, positive limits, and distinct non-nested absolute roots. It
returns the one-member audio delivery set, resolved utterance schema, and JCS
SHA-256 digest.

Management initialization accepts the same empty schema, materializes the
engine cache, and permits catalog access. `provider.validate` reports readiness
of the embedded engine and cache.

The provider never writes `data_dir`. On first initialization it atomically
materializes embedded data under:

```text
<cache_dir>/espeak-ng-1.53.0-data-v1/espeak-ng-data/
```

This bounded cache is reconstructible from the executable and contains no
configuration, license acceptance, input text, or generated audio. Extraction
uses `<cache_dir>/.utterpipe-espeak-ng.lock`, a unique temporary sibling,
required-file and version-marker validation, and atomic publication. Failed
extraction cleans only its own temporary tree.

Any number of provider processes may share the published read-only cache. Each
process serves one host session and one active synthesis; it is not a daemon or
cross-client multiplexer.

## 6. Synthesis and worker lifecycle

The provider accepts nonempty text without NUL, up to 4,096 Unicode scalar
values and the host's lower limit. It caps output at the lower of 256 MiB and
the host limit.

Each synthesis starts the same executable's private `engine-worker` command.
Cache path, voice, effective controls, and output limit are arguments. Plain
synthesis text is sent through worker stdin and is never a command argument or
file. Worker stdout contains only the complete WAV; stderr is not copied into
protocol responses.

The parent reads bounded output, validates the WAV, and reaps the worker.
Cancellation, timeout, overflow, host EOF, and shutdown terminate and reap it.
This process boundary supplies portable hard cancellation around eSpeak's
synchronous native API.

Only one synthesis may be outstanding. Another receives `busy`. Accepted
cancellation stops further output, returns the cancel response, and terminates
the original request with `cancelled` under UtterPipe's stdout linearization
rule. If terminal success was already written, later cancellation is false.
Shutdown terminates an active request before acknowledging shutdown.

The process normally lives for the host session and has no idle timeout.

## 7. Errors, privacy, and network policy

| Condition | UtterPipe code |
| --- | --- |
| Nonempty fixed options | `invalid_provider_options` |
| Invalid per-call control or unavailable voice | `invalid_utterance_options` |
| Empty, NUL-containing, or oversized text | `invalid_text` |
| Active synthesis | `busy` |
| Deadline | `timeout` |
| Output limit | `output_too_large` |
| Missing voice during synthesis | `resource_missing` |
| Engine/cache unavailable | `engine_unavailable` |
| Accepted cancellation | `cancelled` |
| Other engine/audio failure | `synthesis_failed` |

The provider performs no network I/O and needs no credential. It MUST NOT
persist or log synthesis text or audio. Process listings may expose non-secret
voice, controls, limits, and cache paths but never text.

`runtime.health`, `provider.validate`, catalog reads, and direct diagnostics are
local. Stdout contains only framed protocol output in protocol mode.

## 8. Licensing and releases

The provider and embedded engine/data are GPL-3.0-or-later. Catalog items use
license ID `gpl-3.0-or-later`, the pinned upstream license URL, and
`requires_acceptance:false`.

Every binary distribution MUST include the GPL text, provider source location,
pinned engine revision, data-crate provenance, third-party notices, checksum,
and target identity. Release source must be sufficient to rebuild the binary,
including the initialized upstream submodule. Release archives should include
an SBOM.
