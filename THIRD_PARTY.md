# Third-party components

## eSpeak NG

- Project: <https://github.com/espeak-ng/espeak-ng>
- Revision: `359f5f397b85baf875089d3af9cda946bef31dcb`
- License: GPL-3.0-or-later
- Use: statically linked speech engine, Klatt implementation, and speechPlayer
- License text: [`LICENSE`](LICENSE); the corresponding source also contains
  the matching upstream `vendor/espeak-ng/COPYING`

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

Rust dependency versions and checksums are recorded in `Cargo.lock`. The exact
Rust dependency inventory, copyright notices, and license texts for every
release target are reproduced in
[`THIRD_PARTY_LICENSES.html`](THIRD_PARTY_LICENSES.html), with a machine-readable
inventory in each release SBOM.

Every binary release also publishes a corresponding-source archive beside the
binaries. It contains the provider source; the pinned 1.53.0 engine source and
build scripts; a separate eSpeak NG 1.52.0 source tree for the embedded data;
the exact three `0.1.0` data-crate source packages; and the complete
GPL-3.0-or-later text. The release job verifies the 1.52.0 source commit and
obtains the crate packages selected by `Cargo.lock` before packaging them.

Regenerate and compare the Rust report with cargo-about 0.9.1:

```sh
cargo about generate --locked --offline --fail --all-features \
  about.hbs --output-file THIRD_PARTY_LICENSES.generated.html
tr -d '\r' < THIRD_PARTY_LICENSES.generated.html > THIRD_PARTY_LICENSES.normalized.html
cmp THIRD_PARTY_LICENSES.html THIRD_PARTY_LICENSES.normalized.html
```
