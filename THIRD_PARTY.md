# Third-party components

## eSpeak NG

- Project: <https://github.com/espeak-ng/espeak-ng>
- Revision: `359f5f397b85baf875089d3af9cda946bef31dcb`
- License: GPL-3.0-or-later
- Use: statically linked speech engine, Klatt implementation, and speechPlayer
- License text: [`vendor/espeak-ng/COPYING`](vendor/espeak-ng/COPYING)

The build disables eSpeak NG's libpcaudio, libsonic, asynchronous queue, and
MBROLA runtime integrations.

## Embedded eSpeak NG data crates

- Project: <https://github.com/eugenehp/espeak-ng-rs>
- Packages: `espeak-ng-data-phonemes` 0.1.0,
  `espeak-ng-data-dicts` 0.1.0, `espeak-ng-data-dict-ru` 0.1.0
- License: GPL-3.0-or-later
- Use: compiled phoneme tables, language/voice definitions, MBROLA mappings,
  and 114 language dictionaries embedded in the provider executable

These packages redistribute compiled eSpeak NG data. They do not provide the
speech-engine implementation used by this provider.

Rust dependency versions and checksums are recorded in `Cargo.lock`. Release
automation must generate a complete dependency notice/SBOM rather than treating
this short source note as exhaustive.
