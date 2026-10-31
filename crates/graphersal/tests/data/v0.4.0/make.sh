#!/bin/sh
# Regenerates the golden fixtures of Graphersal 0.4.0 (store format version 3, embedding models
# kept as files under `models/<id>/`) with a RELEASED 0.4.0 binary, for the migration tests of
# the store format version 4 (crates/graphersal/tests/all/store_upgrade_tests.rs: the models
# become blobs referenced by catalog definitions). The tests pin what is written below, so
# regenerate them only on purpose. Needs python3 (it writes the tiny model's files).
#
#   usage: make.sh <path to the graphersal 0.4.0 binary> [<output dir, default: this dir>]
#
# Build the binary from the release tag in a separate checkout, for example:
#   git worktree add /tmp/v040 v0.4.0 && (cd /tmp/v040 && cargo build -p graphersal-cli)
#   -> /tmp/v040/target/debug/graphersal
set -eu
BIN=${1:?usage: make.sh <graphersal 0.4.0 binary> [<out dir>]}
OUT=${2:-$(cd "$(dirname "$0")" && pwd)}
"$BIN" --version | grep -q '^graphersal 0\.4\.0$' || { echo "not a 0.4.0 binary: $BIN" >&2; exit 2; }
cd "$OUT"
rm -rf store single.gstore backup backup.zip snapshot.gsnap missing work
mkdir work

# A TINY Model2Vec model (a word-level tokenizer of four tokens, an f32 matrix of 4 x 3; the
# same files as tests/all/model_fixture.rs writes) in work/<name>; `seed` changes the weights.
tiny() {
  python3 - "work/$1" "$2" <<'EOF'
import json, os, struct, sys
d, seed = sys.argv[1], int(sys.argv[2])
os.makedirs(d, exist_ok=True)
open(f"{d}/config.json", "w").write('{"normalize": true}')
vocab = {"[UNK]": 0, "graph": 1, "vector": 2, "model": 3}
tok = {"version": "1.0", "truncation": None, "padding": None, "added_tokens": [],
       "normalizer": {"type": "Lowercase"}, "pre_tokenizer": {"type": "Whitespace"},
       "post_processor": None, "decoder": None,
       "model": {"type": "WordLevel", "vocab": vocab, "unk_token": "[UNK]"}}
open(f"{d}/tokenizer.json", "w").write(json.dumps(tok))
data = b"".join(struct.pack("<f", 1.0 + i + seed * 0.5) for i in range(12))
h = json.dumps({"embeddings": {"dtype": "F32", "shape": [4, 3], "data_offsets": [0, len(data)]}})
while len(h) % 8:
    h += " "
open(f"{d}/model.safetensors", "wb").write(struct.pack("<Q", len(h)) + h.encode() + data)
open(f"{d}/README.md", "w").write("---\nlicense: mit\n---\n# A tiny model\n")
EOF
}
tiny tiny 0
tiny other 1
run() { "$BIN" --graph "$1" -e "$2" > /dev/null; }

# The graph the vectors are made from: `doc` vertices embedded with the stored model acme/tiny
# (0.4.0's embed_property records only `embedder`, never the source property), `person`
# vertices embedded with the built-in hash-ngram/1, `note` vertices with the model imported
# WITHOUT a repository name (identity model2vec@<12 hex>).
DATA='g.add_v("doc").property("text", "graph vector").iterate();
g.add_v("doc").property("text", "model graph").iterate();
g.add_v("doc").property("text", "vector").iterate();
g.add_v("person").property("name", "ada").iterate();
g.add_v("person").property("name", "grace").iterate();
g.add_v("note").property("body", "graph").iterate();
g.add_v("doc").property("title", "no text yet").iterate();
g.set_schema(g.infer_schema(), SchemaMode.open);
g.embed_property(#{label: "doc", from: "text", to: "emb", embedder: "acme/tiny"});
g.embed_property(#{label: "person", from: "name", to: "emb", embedder: "hash-ngram/1"});'

# store/: a directory store with two models (acme/tiny, and one without a repository), the
# vectors above, a checkpoint `base` holding all of it, and the mark `last`.
"$BIN" store create store > /dev/null
"$BIN" embed-model import work/tiny --into store --repo acme/tiny --license mit > /dev/null 2>&1
"$BIN" embed-model import work/other --into store --license mit > /dev/null 2>&1
LOCAL=$("$BIN" --graph store -e 'g.embedders().filter(|e| e.name != "hash-ngram/1" && e.name != "acme/tiny").map(|e| e.name)[0]' 2>/dev/null | tr -d '"')
run store "$DATA g.embed_property(#{label: \"note\", from: \"body\", to: \"emb\", embedder: \"$LOCAL\"});"
"$BIN" store checkpoint store --name base > /dev/null
"$BIN" store mark store last > /dev/null

# A full backup directory, a ZIP backup and a packed snapshot (with the model rows) of store/.
"$BIN" store backup store backup --full > /dev/null
"$BIN" store backup store backup.zip --zip > /dev/null
"$BIN" store export store snapshot.gsnap > /dev/null

# single.gstore: a single-file store with acme/tiny and the doc/person vectors (in the WAL).
"$BIN" store create single.gstore > /dev/null
"$BIN" embed-model import work/tiny --into single.gstore --repo acme/tiny --license mit > /dev/null 2>&1
run single.gstore "$DATA"

# missing/: a store whose schema names a model the store does not keep (0.4.0 served it
# through --embedder).
"$BIN" store create missing > /dev/null
run missing 'g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{emb: #{type: "array", format: "vector", dimension: 3, embedder: "model2vec/acme/gone@0123456789ab"}}}}}})'

# The lock files carry no data (an open recreates them); the model sources are not fixtures.
rm -rf work
rm -f store/LOCK store/BACKUP backup/LOCK backup/BACKUP missing/LOCK missing/BACKUP
echo "fixtures written to $OUT"
