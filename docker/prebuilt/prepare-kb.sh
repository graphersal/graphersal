#!/usr/bin/env bash
# Prepares the `kb` build context of docker/prebuilt/Dockerfile: a compact copy of a Store and
# the MCP configuration (book: "Running in Docker", section "A knowledge base image").
#
#   docker/prebuilt/prepare-kb.sh <store> <out-dir> --bin <cache-dir> [--config F] [--instructions F]
#
# <store>          a Store: a single file (*.gstore) or a directory. Read only, never changed;
#                  it may be in use by a server (the copy is its last durable state).
# <out-dir>        the build context to write (created; must not hold graph.gstore yet):
#                    graph.gstore            `graphersal store fork <store> graph.gstore`: the
#                                            latest state as ONE snapshot, no WAL history, no
#                                            attic, no marks, no free space, the embedding models
#                                            carried, closed cleanly (a new store id and lineage)
#                    mcp-config.json         --config (default: a minimal read-only config)
#                    <instructions file>     --instructions, under its own name (*.md or *.txt;
#                                            the config's instructions.file names it)
#                    store-format-version    the store's format version, for the image label
#                    mcp-cost.out            what the MCP prompt costs an agent (graphersal mcp-cost)
# --bin <dir>      the cache directory of docker/prebuilt/fetch-binary.sh (also GRAPHERSAL_BIN).
#                  The copy is made by THE SAME binary that goes into the image (an older binary
#                  need not open a newer store): the binary of the Docker host's platform runs in
#                  a container of the image's base, never a `graphersal` installed on this host.
#
# The copy is checked (`store verify`) and the configuration is loaded against it (`mcp-cost`)
# before the script succeeds. Environment: PREPARE_KB_MEMORY (container memory, default 4g).
set -euo pipefail

base_image="gcr.io/distroless/static-debian12:nonroot"

usage() {
    echo "usage: $0 <store> <out-dir> --bin <cache-dir> [--config F] [--instructions F]" >&2
    exit 2
}
fail() {
    echo "prepare-kb: $*" >&2
    exit 1
}
abs() {
    (cd "$(dirname "$1")" && printf '%s/%s\n' "$(pwd -P)" "$(basename "$1")")
}

[ $# -ge 2 ] || usage
store="$1"
out="$2"
shift 2
bin="${GRAPHERSAL_BIN:-}"
config=""
instructions=""
while [ $# -gt 0 ]; do
    case "$1" in
    --bin) [ $# -ge 2 ] || usage; bin="$2"; shift 2 ;;
    --config) [ $# -ge 2 ] || usage; config="$2"; shift 2 ;;
    --instructions) [ $# -ge 2 ] || usage; instructions="$2"; shift 2 ;;
    *) usage ;;
    esac
done
[ -n "$bin" ] || fail "--bin <cache-dir> is required (the output of docker/prebuilt/fetch-binary.sh)"
[ -e "$store" ] || fail "no store at $store"
[ -z "$config" ] || [ -f "$config" ] || fail "no config file $config"
if [ -n "$instructions" ]; then
    [ -f "$instructions" ] || fail "no instructions file $instructions"
    case "$instructions" in *.md | *.txt) ;; *) fail "the instructions file must end in .md or .txt (the Dockerfile copies those)" ;; esac
fi

arch="$(docker version -f '{{.Server.Arch}}')" || fail "docker is not available"
case "$arch" in amd64 | arm64) ;; *) fail "unsupported Docker platform $arch" ;; esac
binary="$bin/linux-$arch/graphersal"
[ -x "$binary" ] || fail "no $binary: run docker/prebuilt/fetch-binary.sh <source> $arch first"

mkdir -p "$out"
[ ! -e "$out/graph.gstore" ] || fail "$out/graph.gstore exists: remove it (or use another directory)"
store_abs="$(abs "$store")"
out_abs="$(abs "$out")"
binary_abs="$(abs "$binary")"
name="$(basename "$store_abs")"

# graphersal inside the base image: the store read-only, only the output writable, no network.
run() {
    docker run --rm --network none --memory "${PREPARE_KB_MEMORY:-4g}" \
        --user "$(id -u):$(id -g)" \
        -v "$binary_abs:/usr/local/bin/graphersal:ro" \
        -v "$store_abs:/in/$name:ro" \
        -v "$out_abs:/out" \
        --entrypoint /usr/local/bin/graphersal "$base_image" "$@"
}

echo "prepare-kb: graphersal $(run --version 2>/dev/null | head -n 1) (linux-$arch)" >&2
echo "prepare-kb: forking the latest state of $store into $out/graph.gstore" >&2
run store fork "/in/$name" /out/graph.gstore >&2
run store verify /out/graph.gstore >&2
info="$(run store info /out/graph.gstore)"
echo "$info" >&2
format="$(echo "$info" | awk '$1 == "format" && $2 == "version" { print $3; exit }')"
[ -n "$format" ] || fail "store info names no format version"
echo "$format" >"$out/store-format-version"

if [ -n "$config" ]; then
    cp "$config" "$out/mcp-config.json"
else
    cat >"$out/mcp-config.json" <<'EOF'
{
  "_comment": "Written by prepare-kb.sh: every reading tool."
}
EOF
fi
[ -z "$instructions" ] || cp "$instructions" "$out/$(basename "$instructions")"
chmod a+r "$out"/*

# The configuration loads against the copy (unknown tools, a missing instructions file and a
# writing tool are errors) and the prompt is measured.
run mcp-cost --graph /out/graph.gstore --read-only --mcp-config /out/mcp-config.json \
    >"$out/mcp-cost.out" || fail "the MCP configuration does not load: see the error above"
tail -n 3 "$out/mcp-cost.out" >&2
echo "prepare-kb: $(du -h "$out/graph.gstore" | awk '{print $1}') graph.gstore, store format version $format" >&2
echo "$out_abs"
