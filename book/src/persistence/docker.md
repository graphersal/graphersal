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
`STOPSIGNAL SIGTERM` and port 8080. No image is published; build it from a checkout.

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
graphersal --graph my.gstore -e '{ g.set_schema(g.infer_schema(), SchemaMode.open); g.embed_property(#{label: "doc", from: "text", to: "emb", embedder: "minishlab/potion-base-8M"}).model }'
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
Without `--read-only`, a Store that cannot be opened for writing (no permission, a read-only
file system) is served read-only anyway, with a note on stderr saying so.

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
independent and can scale. A read-write Store belongs to one pod (`replicas: 1`, a
`ReadWriteOnce` volume): its lock refuses a second writer.

The server counts a Kubernetes pod (`KUBERNETES_SERVICE_HOST` set) as a container and listens
on `0.0.0.0`; the image passes `--host 0.0.0.0` anyway. Requests must name the server by an IP
address, `localhost`, a name without a dot (a Service name such as `graphersal`) or the
`--host` name: a fully qualified name such as `graphersal.default.svc.cluster.local` or an
Ingress host is refused with 403 (DNS-rebinding protection, see
[Listening address and Docker](dev_server.md#listening-address-and-docker)). A reverse proxy in
front of it sets `Host` to an accepted name (nginx: `proxy_set_header Host localhost;`) and must
not forward a foreign `Origin`. `--memory-limit auto` bounds every query by the container's
memory limit ([Resource Limits](../users_guide/resource_limits.md)).

## Stopping

`docker stop` and Kubernetes send SIGTERM. The server is PID 1 in the container, where a process
without a signal handler ignores SIGTERM (and is killed after the grace period); the server
handles it:

- without a Store (or read-only): the running query is cancelled, `/readyz` turns 503, and the
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
