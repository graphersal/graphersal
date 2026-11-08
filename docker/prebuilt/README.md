# A knowledge base image from prebuilt binaries

These scripts build a self-contained Docker image that serves one Graphersal Store read-only,
with a read-only MCP endpoint and the playground UI. Everything is baked in: the `graphersal`
binary, the graph (`graph.gstore`) and the MCP configuration (`mcp-config.json`). Nothing is
compiled. The binary is a static Linux build downloaded from GitHub into a local cache.

| File | What it does |
|---|---|
| `fetch-binary.sh` | Downloads the Linux binaries (amd64, arm64) of a release or CI run into a local cache, verifies them, and prints the cache directory. |
| `prepare-kb.sh` | Writes the `kb` build context: a compact copy of your Store, the MCP config, the instructions file, and a prompt-size report. |
| `Dockerfile` | Builds the image from the two contexts (`bin`, `kb`). |

Full documentation: the book, "Running in Docker", section "A knowledge base image"
(`book/src/persistence/docker.md`).

> **No authentication.** Anyone who reaches the port can read the whole knowledge base through
> MCP and the playground. Keep the port on a private network, or put a reverse proxy in front of
> it. The proxy terminates TLS, closes idle connections and caps their number. Pass its public
> host names in `GRAPHERSAL_ALLOWED_HOSTS`.

## Requirements

- Docker with BuildKit/buildx (Docker Desktop is enough). Use `docker buildx create --use` if
  the default builder cannot build multi-platform images.
- `curl` to download a release. To download a CI run you also need `gh`, logged in.
- A Store holding your graph: a `*.gstore` file or a store directory. Its embedding models are
  carried into the image.

## 1. Get the binary

```bash
bin=$(docker/prebuilt/fetch-binary.sh v0.6.0)          # a GitHub Release, both architectures
bin=$(docker/prebuilt/fetch-binary.sh v0.6.0 arm64)    # one architecture only
bin=$(docker/prebuilt/fetch-binary.sh run:42256676106) # a run of .github/workflows/binaries.yml (before a release)
```

The cache lives in `${GRAPHERSAL_CACHE:-~/.cache/graphersal}/bin/<v<version>|run-<id>>/`. A
cached binary whose checksum still matches is reused without downloading, so later builds also
work offline. If the checksum does not match, the binary is deleted and the script fails; run it
again to download it anew. Only the cache directory goes to stdout.

## 2. Prepare the knowledge base context

Write the MCP configuration. The keys are documented in the book, "Dev Server and MCP", section
"The MCP configuration file". A typical knowledge-base setup:

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

Each saved query in the folder `kb` becomes a tool of its own. A config with inline instructions
(`"text": "..."`) is in `crates/graphersal-cli/examples/mcp-kb-readonly.json`.

```bash
docker/prebuilt/prepare-kb.sh my.gstore kb-out --bin "$bin" \
  --config mcp-config.json --instructions kb-instructions.md
cat kb-out/mcp-cost.out      # what the MCP prompt costs an agent (instructions + tools, ~tokens)
```

- **The copy.** It is made with `graphersal store fork`: the latest state only, without history,
  WAL or free space, with the models included. A 2.2 GiB working store became 565 MiB. Your
  Store is only read, and it may be in use by a running server.
- **Same binary as the image.** The copy is made by the binary that goes into the image, run in
  a container, so the image can always open its store.
- **Checks.** The script runs `store verify` on the copy and loads the config against it with
  `mcp-cost`, so a typo in a tool or folder name fails here, not at container start.
- **Without `--config`** you get a minimal read-only config.
- **Instructions file.** It must end in `.md` or `.txt`, and the config's `instructions.file`
  names it.
- **`kb-out` must be new.** It must not already contain a `graph.gstore`.

## 3. Build the image

```bash
docker buildx build --platform linux/amd64,linux/arm64 \
  --build-context bin="$bin" --build-context kb=kb-out \
  --build-arg GRAPHERSAL_VERSION="$(cat "$bin/VERSION")" \
  --build-arg GRAPHERSAL_SOURCE="$(cat "$bin/SOURCE")" \
  --build-arg STORE_FORMAT_VERSION="$(cat kb-out/store-format-version)" \
  --build-arg BUILD_TIME="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  -f docker/prebuilt/Dockerfile -t registry.example.com/my-kb:2026-11-06 --push docker/prebuilt
```

- **Testing locally.** Build only your own platform and load it into Docker instead of pushing:
  `--platform linux/arm64 --load` (use `linux/amd64` on an Intel/AMD machine).
- **Speed.** A build takes seconds: nothing is compiled. The layers go from rarely to often
  changed (binary, config, store), so a knowledge-base update rebuilds and ships only the store
  layer.
- **Labels.** The image records the binary version and source and the store format in the labels
  `dev.graphersal.*`: `docker inspect`.

## 4. Run it

```bash
docker run --rm --read-only -p 127.0.0.1:8080:8080 my-kb:2026-11-06
```

- **Address.** The server listens on `0.0.0.0:8080` inside the container. Choose the host side
  with `-p`.
- **Writes.** It writes nothing, so `--read-only` (a read-only root filesystem) is fine.
- **Stopping.** SIGTERM drains: running queries get 20 s to finish. Use `docker stop -t 30`.
- **Readiness.** The health check turns `healthy` once the store and its model are loaded (a few
  seconds; start period 60 s). For a much larger store raise it with `--health-start-period`.
- **Environment variables:**

  | Variable | Purpose |
  |---|---|
  | `GRAPHERSAL_ALLOWED_HOSTS=kb.example.com,kb` | public names the server answers to behind a proxy (a fully qualified name is refused otherwise) |
  | `GRAPHERSAL_MCP_SESSION_KEY=<32+ random bytes>` | the same value on every replica behind a load balancer, so an MCP session survives switching replicas |
  | `GRAPHERSAL_LOG=info` | diagnostics on stderr |

- **Other flags.** To change one (for example `--workers`, `--queue`,
  `--max-inflight-per-client`, `--timeout`, `--max-materialized-bytes`), repeat the whole command
  after the image name:

  ```bash
  docker run --rm --read-only -p 8080:8080 my-kb:2026-11-06 \
    --graph /kb/graph.gstore --read-only --server --host 0.0.0.0 --port 8080 \
    --mcp --mcp-read-only --mcp-config /kb/mcp-config.json --workers 4
  ```

## 5. Connect an agent

```bash
claude mcp add --transport http kb http://localhost:8080/mcp     # Claude Code
```

opencode (`opencode.json`):

```json
{ "mcp": { "kb": { "type": "remote", "url": "http://localhost:8080/mcp" } } }
```

The playground is at `http://localhost:8080/`. Usage counters (memory only, lost on restart) are
at `http://localhost:8080/api/mcp/stats` and behind the MCP badge in the playground.

## Updating the knowledge base

Re-run steps 2 and 3 with the new Store and a new tag. The binary comes from the cache. Only the
store layer changes.
