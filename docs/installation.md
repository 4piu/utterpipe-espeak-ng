# Installation and maintenance

## Supported release platforms

Published archives currently cover:

- macOS on Apple silicon (`aarch64-apple-darwin`)
- macOS on Intel (`x86_64-apple-darwin`)
- Linux x86-64 with glibc (`x86_64-unknown-linux-gnu`)
- Windows x86-64 (`x86_64-pc-windows-msvc`)

Runtime use does not require a compiler, CMake, a shared eSpeak NG library, or
system eSpeak data.

## Installer behavior

On macOS and Linux, the installer defaults to `$XDG_BIN_HOME`, or
`$HOME/.local/bin` when that variable is unset:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-espeak-ng/main/install.sh | sh
```

On Windows, the PowerShell installer uses a per-user binary directory:

```powershell
irm https://raw.githubusercontent.com/4piu/utterpipe-espeak-ng/main/install.ps1 | iex
```

Both installers select the current platform archive, download its matching
`.sha256` file, verify the archive, and install the provider executable. The
same command performs an initial installation, reinstalls a damaged executable,
or updates to the latest release. Stop running provider instances first;
Windows cannot replace an executable that is in use.

The provider materializes its embedded, read-only engine data into a bounded
cache on first initialization. This is reconstructible data, not configuration
or a separately downloaded voice pack, and ordinary updates preserve it.

Run these checks after installation or an update:

```sh
utterpipe-espeak-ng info
utterpipe-espeak-ng doctor
utterpipe-espeak-ng voices
```

`doctor` verifies the embedded engine and voice catalog. `voices` lists the
bundled voices. `protocol --stdio` is the machine entry point for UtterPipe
hosts; its standard output is reserved for protocol frames.

## Manual installation

Download the archive and matching `.sha256` file from
[GitHub Releases](https://github.com/4piu/utterpipe-espeak-ng/releases). Verify
the checksum with `sha256sum -c` or `shasum -a 256 -c`, extract the archive,
and place `utterpipe-espeak-ng` (`.exe` on Windows) beside the host application
or in a directory on its `PATH`.

Keep the included `LICENSE`, `THIRD_PARTY.md`, `THIRD_PARTY_LICENSES.html`, and
`docs` directory with redistributed binaries.

## Uninstall

Remove only the executable while preserving the reconstructible cache:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-espeak-ng/main/install.sh | sh -s -- --uninstall
```

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/4piu/utterpipe-espeak-ng/main/install.ps1))) -Uninstall
```

Add `--purge` or `-Purge` only to irreversibly remove the provider cache as
well. Purging does not remove user configuration owned by the host.

## Build from source

Building requires Rust 1.88 or newer, CMake, a C/C++ compiler, and Git. Clone
the pinned eSpeak NG submodule and run the repository checks:

```sh
git clone --recurse-submodules https://github.com/4piu/utterpipe-espeak-ng.git
cd utterpipe-espeak-ng
cargo build --release --locked
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

The result is `target/release/utterpipe-espeak-ng` (`.exe` on Windows).
