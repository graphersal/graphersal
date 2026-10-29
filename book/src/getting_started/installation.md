# Installation

The command-line tool `graphersal` (the one-shot query runner, the REPL, the dev server and the
`graphersal store` commands) comes as a prebuilt binary for Linux, macOS and Windows, or builds
from source with Cargo. The engine is also a Python package and a Rust crate. To only try it,
nothing has to be installed: the [playground](playground_tour.md) runs in the browser.

## Prebuilt binaries

Every release on the [releases page](https://github.com/graphersal/graphersal/releases) has one
archive per platform:

| Platform | Archive |
|---|---|
| Linux x86_64 (any distribution, also Alpine and WSL2) | `graphersal-v<version>-linux-x86_64.tar.gz` |
| Linux aarch64 (ARM64) | `graphersal-v<version>-linux-aarch64.tar.gz` |
| macOS on Apple silicon | `graphersal-v<version>-macos-arm64.tar.gz` |
| Windows x86_64 | `graphersal-v<version>-windows-x86_64.zip` |

There is no prebuilt binary for macOS on Intel after 0.4.0 (its runner made every release
much slower): build it from source with Cargo (below), or open an issue if you need one. The
Python package installs there from the sdist, which needs a Rust toolchain.

Each archive holds one directory of its own name with the `graphersal` binary (`graphersal.exe`
on Windows), the licence files, the third-party licences and `README.md`. The Linux binaries are statically linked, so they run on every distribution; see
[Performance of the prebuilt Linux binaries](#performance-of-the-prebuilt-linux-binaries) before
you benchmark with them.

On Linux and macOS (pick the archive of your platform):

```sh
VERSION=0.2.0
NAME=graphersal-v$VERSION-linux-x86_64
ARCHIVE=$NAME.tar.gz
curl -LO https://github.com/graphersal/graphersal/releases/download/v$VERSION/$ARCHIVE
tar -xzf $ARCHIVE                  # unpacks into the directory $NAME
mkdir -p ~/.local/bin && mv $NAME/graphersal ~/.local/bin/    # or: sudo mv $NAME/graphersal /usr/local/bin/
```

`~/.local/bin` must be on your `PATH`; if it is not, add
`export PATH="$HOME/.local/bin:$PATH"` to your shell's start file (`~/.bashrc`, `~/.zshrc`).

On Windows, in PowerShell:

```powershell
$Version = "0.2.0"
$Name = "graphersal-v$Version-windows-x86_64"
$Archive = "$Name.zip"
Invoke-WebRequest "https://github.com/graphersal/graphersal/releases/download/v$Version/$Archive" -OutFile $Archive
# the archive holds one directory $Name with graphersal.exe
Expand-Archive $Archive -DestinationPath "$env:LOCALAPPDATA\graphersal"
$Dir = "$env:LOCALAPPDATA\graphersal\$Name"
# put the directory on the PATH of your user (takes effect in a new terminal)
[Environment]::SetEnvironmentVariable("Path", [Environment]::GetEnvironmentVariable("Path", "User") + ";$Dir", "User")
```

### Check the download

Each release also has a file `SHA256SUMS` with the checksum of every archive. Download it next to
the archive and check:

```sh
curl -LO https://github.com/graphersal/graphersal/releases/download/v$VERSION/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS      # Linux
shasum -a 256 -c --ignore-missing SHA256SUMS  # macOS
```

In PowerShell, compare `(Get-FileHash $Archive -Algorithm SHA256).Hash` with the archive's line in
`SHA256SUMS` (the case of the letters does not matter).

The archives are built on GitHub Actions and carry a GitHub build-provenance attestation, which
proves that a file was built from this repository by its release workflow. With the
[GitHub CLI](https://cli.github.com):

```sh
gh attestation verify $ARCHIVE --repo graphersal/graphersal
```

## cargo binstall

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), Cargo downloads the prebuilt
archive of your platform instead of compiling:

```sh
cargo binstall graphersal-cli
```

## From source

With a Rust toolchain (1.88 or newer, [rustup](https://rustup.rs)):

```sh
cargo install graphersal-cli      # the binary is `graphersal`
```

On Linux this build uses the system's C library (glibc) and its allocator, so it is the one for
benchmarks and heavy workloads (see the next section). From a clone of the repository,
`cargo build -p graphersal-cli --release` writes `target/release/graphersal`.

## Performance of the prebuilt Linux binaries

The prebuilt Linux binaries are statically linked with musl, which makes them run on any
distribution, including Alpine and WSL2. musl's memory allocator is noticeably slower than glibc's
on allocation-heavy queries (large `group()`, `path()` or `to_list()` results, imports):
measured on
the `large` sample graph, the same queries took 1.07 to 1.39 times as long as with a glibc build
of the same source (the most for `group()` and write-heavy scripts). For benchmarks and heavy workloads, build from source with
`cargo install graphersal-cli`, which uses the system's glibc. The figures in this book come from
source builds. The macOS and Windows binaries have no such penalty.

## Windows Subsystem for Linux

Under WSL, use the Linux binary: `linux-x86_64`, or `linux-aarch64` on ARM Windows. WSL1 is not
supported.

- WSL2 forwards `localhost` to Windows, so a `graphersal --server` started in WSL is reachable
  from the Windows browser at `http://localhost:<port>/` (port `8080` unless you give `--port`).
- Keep a [Store](../persistence/overview.md) in the Linux file system (`~/...`), not under
  `/mnt/c`: the Windows drives are mounted over 9p, where `fsync` is slow, and the store's `LOCK`
  file does not protect against a native Windows `graphersal.exe` opening the same files. Never
  open one store from both at once.

## macOS and Windows security prompts

The binaries are not code-signed.

- **macOS**: Gatekeeper blocks a binary downloaded with a browser ("cannot be opened because the
  developer cannot be verified"). Remove the quarantine flag once:
  `xattr -d com.apple.quarantine ./graphersal`. A download with `curl` or `cargo binstall` is not
  quarantined.
- **Windows**: SmartScreen may warn when you first start `graphersal.exe`; choose **More info**,
  then **Run anyway**.

## Verify

```sh
graphersal --version                      # graphersal 0.2.0
graphersal -e 'g.v().count().next()'      # 6: the vertices of the TinkerPop "modern" sample
```

[The Command Line](cli.md) goes on from here. The dev server (`graphersal --server`) is a demo and
development mode, not for production: see [Dev Server and MCP](../persistence/dev_server.md).

## Python

The Python package (wheels for Python 3.10 and newer):

```sh
pip install graphersal
```

See the [Python Quick Start](../library/python.md).

## Rust library

```sh
cargo add graphersal --features script,io,display
```

`script` is the Gremlin DSL as text, `io` GraphSON and GraphML files, `display` results as
tables; add `persist` to keep a graph on disk. [Library Features](../users_guide/installation.md)
lists every feature, and the [Rust Quick Start](../library/rust.md) is the first program.
