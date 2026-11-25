# A knowledge base image from prebuilt binaries

One command builds a Docker image that serves one Graphersal Store **read-only** over HTTP: MCP
at `/mcp` and the playground at `/`. The image holds everything it needs:

- the prebuilt static Linux `graphersal` binary (nothing is compiled),
- your prepared Store file, unchanged (`graph.gstore`),
- the MCP configuration and its Markdown instructions for agents.

It needs no volume and writes nothing, so it runs with a read-only root file system. It
builds and runs with Docker or Podman.

Copy this folder into your own repository, put the store (and optionally the binary) next to it,
and run `build.py`. It works on Linux, macOS and Windows, and in CI. It can also run without
access to GitHub: the binary can come from the repository itself.

> **No authentication.** Anyone who reaches the port can read the whole knowledge base. Keep the
> port on a private network or behind a reverse proxy (TLS, idle timeout, connection cap). Pass
> the proxy's public host names in `GRAPHERSAL_ALLOWED_HOSTS`.

| File | What it does |
|---|---|
| `build.py` | Finds the binary, store and config, writes the build context `.context/`, runs `docker buildx build` or `podman build`. |
| `Dockerfile` | Copies the binary, the configuration and the store into the image. Nothing runs during the build. |
| `.gitignore` | Ignores `.context/`. |

## Requirements

- Python 3.9 or newer. Only the standard library is used.
- Docker with buildx (Docker Desktop, or Docker Engine with the buildx plugin), **or** Podman
  (on macOS and Windows with a running `podman machine`).
- A prepared single-file Store (`*.gstore`) holding your graph. It goes into the image exactly
  as given, so prepare it first (see [Preparing the store](#preparing-the-store)).

## Quick start

Put the files next to `build.py`:

```text
docker/prebuilt/
  build.py  Dockerfile  README.md  .gitignore
  my-kb.gstore                                  your store (exactly one *.gstore)
  graphersal-v0.7.1-linux-x86_64.tar.gz         the binary: a release archive, unpacked
  mcp-config.json                               optional: the MCP configuration
  instructions.md                               optional: instructions for agents
```

Then build and run:

```bash
python docker/prebuilt/build.py -t my-kb:1
docker run --rm --read-only -p 127.0.0.1:8080:8080 my-kb:1
claude mcp add --transport http kb http://localhost:8080/mcp
```

(`python3` on Linux and macOS, `py` on Windows; with Podman, run the image with `podman run`.)
Files can also live elsewhere in your
repository. Then name them with flags:

```bash
python docker/prebuilt/build.py --store data/kb.gstore --binary tools/graphersal \
  --instructions docs/kb-prompt.md -t my-kb:1
```

## Where the binary comes from

Pick one source:

| Source | How |
|---|---|
| In the repository | Release archives `graphersal-v<version>-linux-<x86_64\|aarch64>.tar.gz` or Linux binaries named `graphersal*`, next to `build.py` or in `bin/`. Or name them with `--binary <file or directory>` (repeat it for each architecture). |
| Download | `--version 0.7.1` (or `latest`) downloads the archives of that GitHub Release. |

- **Release archives.** Download them from the
  [releases page](https://github.com/graphersal/graphersal/releases). `x86_64` is for
  `linux/amd64`, `aarch64` for `linux/arm64`. If the release's `SHA256SUMS` lies next to an
  archive, the archive is checked against it.
- **Plain binaries.** The architecture is read from the binary itself, so any name works with
  `--binary`. A `bin/linux-amd64/graphersal`, `bin/linux-arm64/graphersal` layout works too.
- **Downloads.** They are checked against the release's `SHA256SUMS` and cached in
  `~/.cache/graphersal/bin/v<version>/` (`GRAPHERSAL_CACHE` moves it). A cached binary is reused
  without a network connection.
- **CI runs.** Before a release, `--version run:<run-id>` takes the binaries of a run of the
  "CLI binaries" workflow. It needs the GitHub CLI `gh`, logged in.

Use the same version for every architecture. If the versions differ, the build stops.

## The MCP configuration and instructions

Without a configuration, the server offers every reading tool. A typical knowledge base offers
GQL, the schema and the saved queries of one folder as tools of their own (`mcp-config.json`):

```json
{
  "server_name": "my-kb",
  "instructions": { "mode": "replace", "file": "kb-instructions.md" },
  "tools": { "include": ["gql", "get_schema", "run_saved_query"] },
  "saved_query_tools": { "folders": ["kb"], "paging_params": false },
  "resources": ["schema", "saved-queries", "gql-reference"],
  "answer_limit_chars": 30000
}
```

The keys are documented in the Graphersal book, "Dev Server and MCP", section "The MCP
configuration file".

Instructions are a Markdown file. There are two ways to give it:

- **The config names it** (`"instructions": {"file": ...}`). The path is relative to the config
  file, and `build.py` takes the file from there.
- **`--instructions FILE`**, or an `instructions.md` next to `build.py`. It is added after the
  built-in instructions; `--instructions-mode replace` uses it instead of them. A config that
  already sets `"instructions"` cannot be combined with `--instructions`.

The configuration is not checked during the build: a typo in a tool or folder name fails the
container start. Check it beforehand with `graphersal mcp-cost` (see
[Preparing the store](#preparing-the-store)); it also prints what the MCP prompt costs an agent.

## What the build does

1. `build.py` writes the build context `.context/` next to itself: the binaries, the store and
   the config. Large files are hard links where possible, so nothing is copied.
2. The Dockerfile copies them into the image. No binary runs during the build, so building for
   another architecture needs no emulation.
3. The image gets three layers: binary, configuration, store. A new version of the knowledge base
   rebuilds and ships only the store layer. If nothing changed, the build takes seconds.

## Preparing the store

The store goes into the image exactly as given: with its history, WAL and free space, if it has
any. Prepare it once, with your own `graphersal` binary, before you build:

```bash
graphersal store fork live.gstore kb.gstore     # latest state only, one compact file, models carried
graphersal store verify kb.gstore               # every checksum
graphersal mcp-cost --graph kb.gstore --read-only --mcp-config mcp-config.json   # the config loads
```

A 2.2 GiB working store became 565 MiB this way. `store fork` also gives you a consistent copy of
a store a running server is writing; never build from that store itself. A store directory must
be forked into one file first.

## Docker or Podman

`build.py` picks the engine itself: Docker when a Docker with buildx answers, otherwise Podman.
A `docker` command that is Podman's compatibility shim counts as Podman. To choose, pass
`--engine docker` or `--engine podman`, or set `CONTAINER_ENGINE=podman`.

With Podman:

- The image is built with `podman build --format docker`, because the OCI format would drop the
  `HEALTHCHECK`.
- **Local builds hold one platform.** Pass `--push` to build several.
- **`--push` builds a manifest list** under a temporary name, then pushes it to every `-t` tag
  with `podman manifest push --all`. Log in first with `podman login`. A registry without TLS is
  configured in Podman's `registries.conf`, not with `build.py` flags.
- **Older Podman bug.** Podman 4.7 on an arm64 machine marks the amd64 image with the arm64
  variant `v8`, and Docker then finds no `linux/amd64` image. `build.py` sets that entry to `v1`
  (baseline x86-64, which the binary targets) and says so.
- Options after `--` go to `podman build`.

## Platforms, local builds and pushing

| Command | Builds |
|---|---|
| `build.py -t my-kb:1` | Your engine's platform only, kept in the local image store. |
| `build.py -t registry.example.com/my-kb:1 --push` | Every platform you have a binary for, pushed to the registry. |
| `build.py --platform amd64,arm64 ...` | Exactly these platforms. |

- **Downloads.** With `--version` and `--push`, both platforms are downloaded.
- **Keeping two platforms locally** needs Docker Desktop's containerd image store (never with
  Podman). Otherwise use `--push` or build one platform.
- **Extra `docker buildx build` (or `podman build`) options** go after `--`:
  `build.py --push -t ... -- --cache-to type=gha --provenance=true`.
- **A dry run.** `--dry-run` writes `.context/` and prints the build commands without running
  them.

## CI example (GitHub Actions)

No download from GitHub: the release archive is committed to the repository.

```yaml
- uses: docker/setup-qemu-action@v3        # only for a platform the runner is not
- uses: docker/setup-buildx-action@v3
- uses: docker/login-action@v3
  with: { registry: ghcr.io, username: ${{ github.actor }}, password: ${{ secrets.GITHUB_TOKEN }} }
- run: python3 docker/prebuilt/build.py --push -t ghcr.io/${{ github.repository }}-kb:${{ github.sha }}
```

Large stores and archives fit Git LFS well (`git lfs track "*.gstore" "*.tar.gz"`). Check them
out with `actions/checkout` `lfs: true`.

## Running the image

```bash
docker run --rm --read-only -p 127.0.0.1:8080:8080 my-kb:1
curl -s http://localhost:8080/readyz          # {"status":"ready"}
```

- **Address.** The server listens on `0.0.0.0:8080` inside the container. Choose the host side
  with `-p`.
- **Stopping.** SIGTERM drains: running queries get 20 s to finish. Use `docker stop -t 30`.
- **Readiness.** The health check turns `healthy` once the store and its model are loaded. That
  takes seconds; the start period is 60 s. For a much larger store, use
  `docker run --health-start-period 5m`.
- **Environment variables:**

  | Variable | Purpose |
  |---|---|
  | `GRAPHERSAL_ALLOWED_HOSTS=kb.example.com,kb` | public names the server answers to behind a proxy (a fully qualified name is refused otherwise) |
  | `GRAPHERSAL_MCP_SESSION_KEY=<32+ random bytes>` | the same value on every replica behind a load balancer, so an MCP session survives switching replicas |
  | `GRAPHERSAL_LOG=info` | diagnostics on stderr |

- **Other flags** (`--workers`, `--timeout`, `--max-materialized-bytes`, ...). Repeat the whole
  command after the image name:

  ```bash
  docker run --rm --read-only -p 8080:8080 my-kb:1 \
    --graph /kb/graph.gstore --read-only --server --host 0.0.0.0 --port 8080 \
    --mcp --mcp-read-only --mcp-config /kb/mcp-config.json --workers 4
  ```

- **Labels.** `docker inspect` shows the binary's version and source in the labels
  `dev.graphersal.binary.*`.

## Connecting an agent

```bash
claude mcp add --transport http kb http://localhost:8080/mcp     # Claude Code
```

opencode (`opencode.json`):

```json
{ "mcp": { "kb": { "type": "remote", "url": "http://localhost:8080/mcp" } } }
```

The playground is at `http://localhost:8080/`. Usage counters (in memory, lost on restart) are at
`http://localhost:8080/api/mcp/stats`.

## Without build.py

The Dockerfile also builds with plain `docker buildx build` or `podman build --format docker`
from a context laid out like `.context/`:

```text
bin/linux-amd64/graphersal      one binary per platform built
store/graph.gstore              the prepared single-file store, shipped unchanged
kb/mcp-config.json              the configuration ({} for the defaults)
kb/instructions.md              the instructions file the configuration names, if any
```

```bash
docker buildx build --platform linux/amd64 \
  -f docker/prebuilt/Dockerfile -t my-kb:1 --load <context>
```
