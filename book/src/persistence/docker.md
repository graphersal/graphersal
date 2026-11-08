# Running in Docker

> **The dev server has no authentication.** Whoever reaches its port can run every query the
> server allows (all of them without `--read-only`), and with `--ai` spend the AI provider key.
> Keep the port on a private network, publish it on the host's loopback only
> (`-p 127.0.0.1:8080:8080`), or put it behind a reverse proxy that authenticates. It is a
> [dev server](dev_server.md): its HTTP API is EXPERIMENTAL.

The repository has a `Dockerfile` that builds the `graphersal` command line into a small image:
the static Linux binary (the same build as the release binaries: default features, so the dev
server, MCP, the AI Chat and [embedding models](store_models.md) are in) on
`gcr.io/distroless/static`, running as a non-root user (uid 65532), with a `HEALTHCHECK`,
`STOPSIGNAL SIGTERM` and port 8080. No image is published; build it from a checkout. To ship a
knowledge base as one image without compiling anything, see
[A knowledge base image](#a-knowledge-base-image).

## Serve a single-file database read-only with MCP

The default command of the image serves the [single-file Store](single_file_store.md)
`/data/graph.gstore` read-only, with a read-only MCP endpoint for an AI agent:

```text
graphersal --graph /data/graph.gstore --read-only --server --host 0.0.0.0 --port 8080 --mcp --mcp-read-only
```

Build the image, then either mount the file or bake it into an image of your own:

```bash
docker build -t graphersal .                       # in the repository checkout

# (1) the file mounted read-only from the host
docker run --rm -p 127.0.0.1:8080:8080 \
  -v "$PWD/my.gstore:/data/graph.gstore:ro" graphersal

# (2) the file inside an image of your own
cat > Dockerfile.mygraph <<'EOF'
FROM graphersal
COPY my.gstore /data/graph.gstore
EOF
docker build -f Dockerfile.mygraph -t mygraph .
docker run --rm -p 127.0.0.1:8080:8080 mygraph
```

Then `http://localhost:8080/` is the playground and `http://localhost:8080/mcp` the MCP endpoint
(`claude mcp add --transport http graphersal http://localhost:8080/mcp`). An embedding model
kept in the store ([Embedding Models in a Store](store_models.md)) is loaded from the file, so
text queries (`nearest("emb", "some text", 5)`) work without a network. Make the store with the
command line first, for example:

```bash
graphersal --graph my.gstore --create-store -e 'g.add_v("doc").property("text", "a cat on the mat").iterate()'
graphersal embed-model download minishlab/potion-base-8M --into my.gstore
graphersal --graph my.gstore -e 'g.set_schema(g.infer_schema(), SchemaMode.open); g.patch_schema(#{vertices: #{doc: #{schema: #{properties: #{emb: #{type: "array", format: "vector", embedder: "minishlab/potion-base-8M", from: "text"}}}}}})'
```

Pass other arguments after the image name to replace the command (`docker run ... graphersal
--graph /data/graph.gstore --server --host 0.0.0.0 --port 8080`); the entry point is the
`graphersal` binary.

### Read-only

With `--read-only` the served graph refuses every write: queries, schema changes, saved
queries, indexes and compression rules, "Save to file", and the MCP endpoint offers only its
reading tools. A Store is **loaded without its lock and nothing in it is written**: no recovery
cut of an interrupted last write (it is ignored), no checkpoint, no
[workload statistics](workload_statistics.md) file, no temporary file beside it. So all of
these work:

- a `:ro` bind mount or volume;
- a read-only root file system (`docker run --read-only`);
- a file the container's user may not write (the image runs as uid 65532, a file copied into an
  image belongs to root).

The server shows no Store panel and no history then (it serves the latest state as a graph).

`--server --read-only` is the [read-only serving mode](dev_server.md#read-only-serving-mode),
made for many readers: the queries of all browser pages and MCP agents run in parallel on a
pool of workers (`--workers`, default the container's CPUs), behind a bounded queue
(`--queue`, `--queue-timeout`; a full queue answers `503` with `Retry-After: 1`), at most
`--max-inflight-per-client` queries per agent or page and `--max-connections` requests at once,
under safe default limits (SAFE script limits, a 30 s timeout, 500 MiB per query). MCP session
ids are stateless. **It has no authentication**: the server is protected only by being
read-only and by these limits, so publish the port where you trust who can reach it (or put an
authenticating reverse proxy in front). Not available in this mode: the Store panel, history,
scheduled jobs, saving and every write.
Without `--read-only`, a Store that cannot be opened for writing (no permission, a read-only
file system) is served read-only anyway, with a note on stderr saying so.

## A knowledge base image

A knowledge base for AI agents ships best as ONE image: the binary, the Store and the MCP
configuration baked in, served read-only, so an agent connects with a URL and nothing else.
`docker/prebuilt/` builds such an image without compiling anything: the binary is a prebuilt
release binary, downloaded once into a local cache.

> **No authentication.** Whoever reaches the port reads the whole knowledge base, through MCP and
> through the playground, without a token. The server is protected only by being read-only and
> by the limits of the [read-only serving mode](dev_server.md#read-only-serving-mode). Run it on a
> private network or behind a [reverse proxy](#behind-a-reverse-proxy).

### 1. Fetch the binary

```bash
bin=$(docker/prebuilt/fetch-binary.sh v0.6.0)        # both architectures; or: ... v0.6.0 arm64
```

`fetch-binary.sh <source> [amd64|arm64 ...]` downloads the static Linux binaries of a GitHub
Release (`v<version>`: the `graphersal-v<version>-linux-*.tar.gz` assets) and checks them against
the release's `SHA256SUMS`. `run:<run-id>` takes the artifacts of a run of the "CLI binaries"
workflow instead (a build of a branch before its release; needs the GitHub CLI `gh`), checked
against that run's `SHA256SUMS`. The binaries land in
`${GRAPHERSAL_CACHE:-~/.cache/graphersal}/bin/<v<version>|run-<run-id>>/linux-<amd64|arm64>/graphersal`
next to `VERSION`, `SOURCE` and `SHA256SUMS`; the script prints that directory and nothing else
on stdout. A cached binary whose checksum matches is not downloaded again (builds work offline);
one that does not match is deleted and the script fails (run it again).

### 2. Prepare the knowledge base

```bash
docker/prebuilt/prepare-kb.sh my.gstore kb-out --bin "$bin" \
  --config mcp-config.json --instructions kb-instructions.md
```

The image gets a prepared copy of the store, never the live file: `prepare-kb.sh` runs
`graphersal store fork <store> kb-out/graph.gstore`, which writes the latest state as one
snapshot (no write-ahead log history, no attic, no marks, no free space, a new store id), with
the store's [embedding models](store_models.md), closed cleanly; then `store verify` on the copy.
The source is mounted read-only and may be in use by a running server (the copy is its last
durable state). A store of 2.2 GiB (1.7 GiB live: 61 WAL segments of history, a 506 MiB model,
560 MiB free space inside the file) became 565 MiB.

The copy is made by **the same binary that goes into the image**, run in a container of the
image's base (the binary of the Docker host's platform), never by a `graphersal` installed on the
host: an older binary need not open a store written by a newer one, so the image always opens
what it carries. The script writes into `kb-out/`:

| File | What |
|---|---|
| `graph.gstore` | the prepared store |
| `mcp-config.json` | `--config` ([MCP configuration](dev_server.md#the-mcp-configuration-file---mcp-config)); without it every reading tool |
| `kb-instructions.md` | `--instructions`, under its own name (`.md` or `.txt`); the config's `instructions.file` names it |
| `store-format-version` | the store's format version, for the image label |
| `mcp-cost.out` | what the MCP prompt costs an agent (`graphersal mcp-cost`); it also proves that the configuration loads against the copy |

A configuration for a knowledge base offers GQL, the schema and the saved queries of one folder
as tools of their own (the repository's
`crates/graphersal-cli/examples/mcp-kb-readonly.json`):

```json
{
  "server_name": "project-kb",
  "instructions": { "mode": "replace", "file": "kb-instructions.md" },
  "tools": { "include": ["gql", "get_schema", "run_saved_query"] },
  "saved_query_tools": { "folders": ["kb"], "paging_params": false },
  "resources": ["schema", "saved-queries", "gql-reference"],
  "answer_limit_chars": 30000
}
```

With 14 saved queries in the folder `kb` this prompt (instructions and tool list) costs an agent
about 3.3 K tokens, against about 4.6 K for every reading tool without a configuration.

### 3. Build

```bash
docker buildx build --platform linux/amd64,linux/arm64 \
  --build-context bin="$bin" --build-context kb=kb-out \
  --build-arg GRAPHERSAL_VERSION="$(cat "$bin/VERSION")" \
  --build-arg GRAPHERSAL_SOURCE="$(cat "$bin/SOURCE")" \
  --build-arg STORE_FORMAT_VERSION="$(cat kb-out/store-format-version)" \
  --build-arg BUILD_TIME="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  -f docker/prebuilt/Dockerfile -t my-kb:2026-11-06 --push docker/prebuilt
```

`docker/prebuilt/Dockerfile` has no build stage. The binary and the store come in as BuildKit
named contexts (`bin`, `kb`), so neither is sent as a build context, and three layers are copied
onto `gcr.io/distroless/static` (non-root user 65532, everything read-only for it): the binary,
the configuration, the store. A new version of the knowledge base rebuilds and ships only the
last layer. Building both platforms from a warm cache takes seconds (about 7 s when nothing
changed, 25 s with a new store layer; the image is about 560 MB compressed per platform). The image's labels name the
binary (`dev.graphersal.binary.version`, `dev.graphersal.binary.source`) and the store's format
version (`dev.graphersal.store.format-version`); tag the image with the version of the knowledge
base. `--push` sends it to your registry; `--load` instead keeps it in the local image store
(Docker Desktop's containerd store keeps both platforms; with the classic store build one platform
for `--load`).

The command of the image is the read-only serving mode with read-only MCP:

```text
graphersal --graph /kb/graph.gstore --read-only --server --host 0.0.0.0 --port 8080 \
  --mcp --mcp-read-only --mcp-config /kb/mcp-config.json
```

The health check is `graphersal health --ready`: the container turns healthy once the store and
its models are loaded. Its start period is 60 s; the store above (565 MiB, a 506 MiB model) is
ready in about 4 s on a laptop (also under amd64 emulation on an arm64 Mac; the model itself is
loaded at its first use), so raise it only for a much larger knowledge base
(`docker run --health-start-period 5m`, or the probes of your orchestrator).

### 4. Run and connect

```bash
docker run --rm --read-only -p 127.0.0.1:8080:8080 my-kb:2026-11-06
curl -s http://localhost:8080/readyz          # {"status":"ready"}
```

`--read-only` (a read-only root file system) is the intended way: the server writes no file at
all, its usage statistics stay in memory (`GET /api/mcp/stats`), diagnostics go to stderr. Pass
more options by repeating the whole command after the image name (the entry point is the
binary), or set `GRAPHERSAL_ALLOWED_HOSTS` and `GRAPHERSAL_LOG` with `-e`. The limits of the
read-only serving mode (`--workers`, `--queue`, `--timeout`, ...) are the defaults;
`--memory-limit auto` follows the container's memory limit (`docker run --memory`).

Connect an agent with the URL alone:

```bash
claude mcp add --transport http kb http://localhost:8080/mcp          # Claude Code
```

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": { "kb": { "type": "remote", "url": "http://localhost:8080/mcp" } }
}
```

(the second is `opencode.json` for opencode). Behind a proxy the URL is
`https://kb.example.com/mcp`. The playground is at `http://localhost:8080/`.

### Update the knowledge base

Prepare the new state into a fresh directory and build again with a new tag: the binary and
configuration layers come from the cache, only the store layer is new.

```bash
docker/prebuilt/prepare-kb.sh my.gstore kb-out-2 --bin "$bin" --config mcp-config.json \
  --instructions kb-instructions.md
docker buildx build ... --build-context kb=kb-out-2 ... -t my-kb:2026-11-13 --push docker/prebuilt
```

Replace the running container by the new image (`docker stop -t 30` lets running queries finish;
see [Stopping](#stopping)). A new binary is a new `fetch-binary.sh` source; prepare the store
again with it, since the copy must be written by the binary that serves it.

### Behind a reverse proxy

The server speaks plain HTTP. On a network, put a reverse proxy in front of it that:

- ends **TLS** (the server has none);
- closes **idle connections** and **caps the connections** to the server: the server keeps a
  thread per open connection without an idle timeout of its own, and refuses requests beyond
  `--max-connections` (512) with 503;
- absorbs bursts of new connections (the server's listen backlog is 128).

Name the proxy's public host names with `--allowed-host` or `GRAPHERSAL_ALLOWED_HOSTS`
(comma-separated): the server refuses a fully qualified `Host` it does not know with 403
(protection against DNS rebinding), and accepts an `https://` `Origin` of an allowed name, so the
playground works through the proxy ([Safety](dev_server.md#safety)).

```bash
docker run -d --read-only --name kb -p 127.0.0.1:8080:8080 \
  -e GRAPHERSAL_ALLOWED_HOSTS=kb.example.com my-kb:2026-11-06
```

Caddy (`Caddyfile`; certificates automatic):

```text
{
    servers {
        timeouts {
            idle 2m
        }
    }
}

kb.example.com {
    reverse_proxy 127.0.0.1:8080 {
        transport http {
            max_conns_per_host 256
            keepalive 60s
        }
        flush_interval -1
    }
}
```

nginx:

```text
upstream graphersal {
    server 127.0.0.1:8080 max_conns=256;
    keepalive 32;
    keepalive_timeout 60s;
}
limit_conn_zone $binary_remote_addr zone=kb_per_ip:10m;

server {
    listen 443 ssl;
    server_name kb.example.com;
    ssl_certificate     /etc/ssl/kb.example.com.crt;
    ssl_certificate_key /etc/ssl/kb.example.com.key;
    keepalive_timeout 65s;
    client_body_timeout 30s;
    limit_conn kb_per_ip 64;

    location / {
        proxy_pass http://graphersal;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_set_header Host $host;
        proxy_read_timeout 60s;
        proxy_buffering off;
    }
}
```

`max_conns`/`max_conns_per_host` bound the connections to the server (keep them below
`--max-connections`); `limit_conn` stops one client from taking them all. The proxy adds no
authentication here; add one in the proxy if you need it (MCP clients then must send its
credentials).

## Health checks

| Endpoint | Answer | Use |
|---|---|---|
| `GET /healthz` | `200 {"status":"ok"}` while the process answers HTTP: also while the graph loads, a long query runs or the server stops | liveness |
| `GET /readyz` | `200 {"status":"ready"}` once the graph is loaded; `503 {"status":"loading"}` before, `503 {"status":"stopping"}` after SIGTERM | readiness |

Both also answer `HEAD`. They take no lock, never wait for a running query (a long write never
fails liveness), reveal nothing of the graph and are exempt from the Host/Origin checks, so a
Kubernetes probe (`Host: <pod IP>:8080`) and a Docker health check work in every bind mode. The
server listens before it loads its graph: while a large Store opens, `/healthz` answers 200 and
`/readyz` and every other endpoint 503. Probe requests are not logged.

The image has no curl or wget; the binary is its own probe:

```bash
graphersal health                 # GET /healthz on 127.0.0.1:8080; exit 0 when it answers 200
graphersal health --ready         # GET /readyz
graphersal health --host 10.0.0.5 --port 9000 --timeout 1000
```

It prints nothing on success and the reason on stderr otherwise (exit code `1`; `2` for an
invalid invocation). The image's health check:

```dockerfile
HEALTHCHECK --interval=10s --timeout=3s --start-period=30s --retries=3 \
    CMD ["/usr/local/bin/graphersal", "health"]
```

## Kubernetes

```yaml
apiVersion: apps/v1
kind: Deployment
metadata: { name: graphersal }
spec:
  replicas: 1
  selector: { matchLabels: { app: graphersal } }
  template:
    metadata: { labels: { app: graphersal } }
    spec:
      terminationGracePeriodSeconds: 30
      securityContext: { runAsNonRoot: true, runAsUser: 65532, fsGroup: 65532 }
      containers:
        - name: graphersal
          image: mygraph:0.4.0            # an image of your own (see above)
          ports: [{ containerPort: 8080 }]
          securityContext: { readOnlyRootFilesystem: true, allowPrivilegeEscalation: false }
          livenessProbe:
            httpGet: { path: /healthz, port: 8080 }
            periodSeconds: 10
          readinessProbe:
            httpGet: { path: /readyz, port: 8080 }
            periodSeconds: 5
          startupProbe:                   # a large Store takes time to load
            httpGet: { path: /readyz, port: 8080 }
            periodSeconds: 2
            failureThreshold: 150
          resources:
            limits: { memory: 2Gi }
```

Every pod serves its own copy of the graph in memory; with `--read-only` replicas are
independent and can scale. Give them the same `GRAPHERSAL_MCP_SESSION_KEY` (a secret of at
least 32 characters, e.g. from a Kubernetes Secret; never printed) so an agent's MCP session id
from one replica is accepted by every other one behind the Service; without it each process
signs with a random key of its own and an agent switched to another replica initializes again.
A read-write Store belongs to one pod (`replicas: 1`, a
`ReadWriteOnce` volume): its lock refuses a second writer.

The server counts a Kubernetes pod (`KUBERNETES_SERVICE_HOST` set) as a container and listens
on `0.0.0.0`; the image passes `--host 0.0.0.0` anyway. Requests must name the server by an IP
address, `localhost`, a name without a dot (a Service name such as `graphersal`) or the
`--host` name: a fully qualified name such as `graphersal.default.svc.cluster.local` or an
Ingress host is refused with 403 (DNS-rebinding protection, see
[Listening address and Docker](dev_server.md#listening-address-and-docker)) unless it is named
with `--allowed-host` or `GRAPHERSAL_ALLOWED_HOSTS` (an `env` entry of the container): name the
Ingress host there, and the `https://` origin of the playground behind it is accepted too
([Behind a reverse proxy](#behind-a-reverse-proxy)). The process memory ceiling is on by default and follows the
container's memory limit minus a headroom (`--memory-limit auto`, the default; `--memory-headroom`
changes the headroom): a query that would exceed it fails and is rolled back instead of the pod
being OOM-killed ([Resource Limits](../users_guide/resource_limits.md#the-process-memory-ceiling)).
A store whose graph does not fit into the container refuses the OPEN instead (exit 1, nothing
opened or changed, never maintenance mode); the error names the minimum `--memory-limit` (or
container memory) it needs.

## Stopping

`docker stop` and Kubernetes send SIGTERM. The server is PID 1 in the container, where a process
without a signal handler ignores SIGTERM (and is killed after the grace period); the server
handles it:

- `--read-only` (the read-only serving mode): `/readyz` turns 503 and no new query is taken; the
  running queries get `--drain-timeout` seconds (default 20) to finish, then they are cancelled
  and the process exits with code 0 (give the container a grace period above the drain
  timeout: `docker stop -t 30`, `terminationGracePeriodSeconds: 30`);
- without a Store otherwise: the running query is cancelled, `/readyz` turns 503, and the
  process exits at once with exit code 0;
- with a read-write Store: the same, then the store is closed cleanly (its write-ahead log
  synced, marked closed) before the exit; a second signal exits at once (code 130).

`docker stop` returns in well under a second. Give a large read-write Store a grace period that
covers its close (`docker stop -t 30`, `terminationGracePeriodSeconds`). A one-shot script or
the REPL running as PID 1 exits with 130 on SIGINT/SIGTERM.

## A read-write Store in a volume

```bash
docker volume create graphdata
docker run --rm -p 127.0.0.1:8080:8080 -v graphdata:/data graphersal \
  --graph /data/graph.gstore --create-store --server --host 0.0.0.0 --port 8080
```

- A **named volume** mounted at `/data` takes the ownership of the image's `/data` (uid 65532),
  so the non-root server can write it. A **bind mount** of a host directory keeps the host's
  ownership: make it writable for uid 65532 (`chown 65532:65532 dir`) or run with
  `--user $(id -u):$(id -g)`.
- Put the store **inside** the mounted directory (`/data/graph.gstore`, or `/data/graph/` for a
  directory store), not as the mount point itself: a fork of the store (Store panel) is written
  next to it, in the same directory.
- The writer lock is an OS file lock (`flock` on Linux). It works on local volumes and bind
  mounts; Docker Desktop's file sharing (macOS, Windows) and network file systems may not honour
  it between machines. Open a store from one container at a time.
- Without `--read-only` the server writes only into the store (and a `--graph` file on "Save to
  file"): there are no temporary files elsewhere, so `docker run --read-only` works with the
  volume as the one writable place. A tmpfs (`--tmpfs /tmp`) is not needed.

## Logs and configuration

Everything goes to stdout and stderr, nothing to log files: the address line on stdout (written
and flushed once the server serves, so `docker logs` shows it at once), the start banner,
warnings and store messages on stderr. `GRAPHERSAL_LOG=off|error|warn|info|debug|trace` sets the
level of the library's messages (default `warn`): `docker run -e GRAPHERSAL_LOG=info ...`.
`TZ` sets the time zone of [scheduled jobs](scheduled_jobs.md) (distroless has no
`/etc/localtime`, so they run in UTC without it).

Options come from the command line, or from a JSON file with `--args-file FILE` (or `@FILE`;
[Arguments from a file](dev_server.md#arguments-from-a-file)), which fits a ConfigMap or a
mounted file:

```bash
docker run --rm -p 127.0.0.1:8080:8080 -v "$PWD/my.gstore:/data/graph.gstore:ro" \
  -v "$PWD/server.json:/config/server.json:ro" graphersal @/config/server.json
```

```json
{"graph": "/data/graph.gstore", "read-only": true, "server": true, "host": "0.0.0.0",
 "port": 8080, "mcp": true, "mcp-read-only": true, "timeout": 30000, "memory-limit": "auto"}
```

## Building the image

```bash
docker build -t graphersal .
docker build --build-arg RUST_VERSION=1.91 -t graphersal .     # another toolchain
```

The build stage is `rust:<RUST_VERSION>-alpine` (musl, a static binary; profile `dist`, symbols
stripped). It links with thin LTO by default: the fat LTO of the release binaries needs more
memory than the 8 GB of a default Docker Desktop VM (the linker is killed);
`--build-arg LTO=fat` builds exactly the release binary where the builder has the memory.
Dependencies and the target directory live in BuildKit cache mounts, so a source change
rebuilds only the workspace crates. The base image tags in the `Dockerfile` are
examples: bump them when you build. `docker builder prune` removes the build cache.
