#!/bin/sh
# Regenerates the 0.4.0 fixtures whose NEWEST WAL segment holds no commit, only the record of a
# checkpoint taken while the store was open (crates/graphersal/tests/all/store_upgrade_tests.rs,
# module `v040_checkpointed`): such a checkpoint starts a segment named by the NEXT commit and
# writes its Checkpoint record there, so the upgraded store's next commit goes into a segment of
# the older file format version that already holds records. Beside make.sh, whose fixtures it
# leaves alone. Needs python3 (the tiny model's files) and curl.
#
#   usage: make_checkpointed.sh <path to the graphersal 0.4.0 binary> [<output dir>]
set -eu
BIN=${1:?usage: make_checkpointed.sh <graphersal 0.4.0 binary> [<out dir>]}
OUT=${2:-$(cd "$(dirname "$0")" && pwd)}
"$BIN" --version | grep -q '^graphersal 0\.4\.0$' || { echo "not a 0.4.0 binary: $BIN" >&2; exit 2; }
cd "$OUT"
rm -rf checkpointed checkpointed_mark checkpointed_missing work-cp
mkdir work-cp

# The same TINY Model2Vec model as make.sh writes (acme/tiny, identity
# model2vec/acme/tiny@70c61d0ed332).
python3 - work-cp/tiny <<'EOF'
import json, os, struct, sys
d = sys.argv[1]
os.makedirs(d, exist_ok=True)
open(f"{d}/config.json", "w").write('{"normalize": true}')
vocab = {"[UNK]": 0, "graph": 1, "vector": 2, "model": 3}
tok = {"version": "1.0", "truncation": None, "padding": None, "added_tokens": [],
       "normalizer": {"type": "Lowercase"}, "pre_tokenizer": {"type": "Whitespace"},
       "post_processor": None, "decoder": None,
       "model": {"type": "WordLevel", "vocab": vocab, "unk_token": "[UNK]"}}
open(f"{d}/tokenizer.json", "w").write(json.dumps(tok))
data = b"".join(struct.pack("<f", 1.0 + i) for i in range(12))
h = json.dumps({"embeddings": {"dtype": "F32", "shape": [4, 3], "data_offsets": [0, len(data)]}})
while len(h) % 8:
    h += " "
open(f"{d}/model.safetensors", "wb").write(struct.pack("<Q", len(h)) + h.encode() + data)
open(f"{d}/README.md", "w").write("---\nlicense: mit\n---\n# A tiny model\n")
EOF

# The dev server on store $1: POST /api/store/checkpoint (the store is OPEN: the checkpoint
# writes its record into a new WAL segment), then $2 (more curl calls, may be empty), then
# SIGTERM (closes the store).
checkpoint_open() {
  "$BIN" --graph "$1" --server --port 0 > work-cp/address 2> work-cp/server.log &
  SERVER=$!
  for _ in $(seq 1 100); do [ -s work-cp/address ] && break; sleep 0.1; done
  URL=$(grep -o 'http://[^ ]*' work-cp/address | head -1)
  curl -sf -X POST -H 'Content-Type: application/json' --data '{"name": "base"}' \
    "${URL}api/store/checkpoint" > /dev/null
  eval "$2"
  kill -TERM $SERVER
  wait $SERVER || true
  rm -f work-cp/address
}

DATA='g.add_v("doc").property("text", "graph vector").iterate();
g.add_v("doc").property("text", "model graph").iterate();
g.add_v("person").property("name", "ada").iterate();
g.set_schema(g.infer_schema(), SchemaMode.open);
g.embed_property(#{label: "doc", from: "text", to: "emb", embedder: "acme/tiny"});'

# checkpointed/: the model acme/tiny (models/), three vertices with the doc vectors, a
# checkpoint taken while open: the newest segment holds only its Checkpoint record.
"$BIN" store create checkpointed > /dev/null
"$BIN" embed-model import work-cp/tiny --into checkpointed --repo acme/tiny --license mit > /dev/null 2>&1
"$BIN" --graph checkpointed -e "$DATA" > /dev/null
checkpoint_open checkpointed ''

MARK='curl -sf -X POST -H "Content-Type: application/json" --data "{\"name\": \"after\"}" "${URL}api/store/mark" > /dev/null'

# checkpointed_mark/: no model, no schema, two vertices, a checkpoint taken while open and then
# the mark `after` (also while open): the newest segment holds a Checkpoint and a Mark record.
# The upgraded store opens; its first commit goes into that segment.
"$BIN" store create checkpointed_mark > /dev/null
"$BIN" --graph checkpointed_mark -e 'g.add_v("person").property("name", "ada").iterate();
g.add_v("person").property("name", "grace").iterate();' > /dev/null
checkpoint_open checkpointed_mark "$MARK"

# checkpointed_missing/: the same, but with a schema naming acme/tiny, a model the store does
# not keep (0.4.0 served it through --embedder): the upgraded store refuses to open until the
# model is imported, a commit to the CLOSED store, which goes into that segment.
"$BIN" store create checkpointed_missing > /dev/null
"$BIN" --graph checkpointed_missing -e 'g.add_v("person").property("name", "ada").iterate();
g.add_v("person").property("name", "grace").iterate();
g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{emb: #{type: "array", format: "vector", dimension: 3, embedder: "model2vec/acme/tiny@70c61d0ed332"}}}}}})' > /dev/null
checkpoint_open checkpointed_missing "$MARK"

rm -rf work-cp
rm -f checkpointed/LOCK checkpointed/BACKUP checkpointed/WORKLOAD
rm -f checkpointed_mark/LOCK checkpointed_mark/BACKUP checkpointed_mark/WORKLOAD
rm -f checkpointed_missing/LOCK checkpointed_missing/BACKUP checkpointed_missing/WORKLOAD
echo "fixtures written to $OUT"
