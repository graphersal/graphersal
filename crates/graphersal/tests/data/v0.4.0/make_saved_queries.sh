#!/bin/sh
# Regenerates the 0.4.0 fixtures with SAVED QUERIES calling `g.embed_property`, which 0.5.0
# removed (crates/graphersal/tests/all/store_upgrade_tests.rs, module `v040_saved_queries`): the
# upgrade report lists them with the vector rule replacing each call. Beside make.sh, whose
# fixtures it leaves alone. Needs python3 and curl.
#
#   usage: make_saved_queries.sh <path to the graphersal 0.4.0 binary> [<output dir>]
#
# 0.4.0 refused such a body in `g.define_query` (it changes the schema), but kept one that came
# through the catalog file (the dev server's POST /api/catalog/load, the Rust API): that is how
# these queries are stored here.
set -eu
BIN=${1:?usage: make_saved_queries.sh <graphersal 0.4.0 binary> [<out dir>]}
OUT=${2:-$(cd "$(dirname "$0")" && pwd)}
"$BIN" --version | grep -q '^graphersal 0\.4\.0$' || { echo "not a 0.4.0 binary: $BIN" >&2; exit 2; }
cd "$OUT"
rm -rf saved_queries saved_queries.gsnap work-sq
mkdir work-sq

# The store: two docs with a text, an open schema, and one ordinary saved query (so the catalog
# file has 0.4.0's own format).
"$BIN" store create saved_queries > /dev/null
"$BIN" --graph saved_queries -e 'g.add_v("doc").property("text", "graph vector").iterate();
g.add_v("doc").property("text", "model graph").iterate();
g.set_schema(g.infer_schema(), SchemaMode.open);
g.define_query(#{name: "docs", body: "fn docs() { g.v().has_label(\"doc\").values(\"text\") }"})' > /dev/null

# The dev server on the store: export the catalog, add the queries calling embed_property,
# load it back, stop the server (SIGTERM closes the store).
"$BIN" --graph saved_queries --server --port 0 > work-sq/address 2> work-sq/server.log &
SERVER=$!
for _ in $(seq 1 100); do [ -s work-sq/address ] && break; sleep 0.1; done
URL=$(grep -o 'http://[^ ]*' work-sq/address | head -1)
curl -sf "${URL}api/catalog/export" > work-sq/catalog.json
python3 - work-sq/catalog.json <<'EOF'
import json, sys
path = sys.argv[1]
catalog = json.load(open(path))
entries = catalog["definitions"]
template = next(e for e in entries if e["name"] == "docs")
def query(name, body):
    entry = json.loads(json.dumps(template))
    entry["name"] = name
    entry["source"] = body
    entry.pop("description", None)
    return entry
entries.append(query("fill", 'fn fill() { g.embed_property(#{label: "doc", from: "text", to: "emb", embedder: "hash-ngram/1"}) }'))
entries.append(query("fill_any", 'fn fill_any() { let l = "doc"; g.embed_property(#{label: l, from: "text", to: "emb"}) }'))
json.dump(catalog, open(path, "w"))
EOF
curl -sf -X POST -H 'Content-Type: application/json' --data-binary @work-sq/catalog.json \
  "${URL}api/catalog/load" > /dev/null
kill -TERM $SERVER
wait $SERVER || true

# A packed snapshot of it (export packs the latest stored snapshot: a checkpoint first).
"$BIN" store checkpoint saved_queries > /dev/null
"$BIN" store export saved_queries saved_queries.gsnap > /dev/null

rm -rf work-sq
rm -f saved_queries/LOCK saved_queries/BACKUP saved_queries/WORKLOAD
echo "fixtures written to $OUT"
