#!/bin/sh
# Runs the Rust code blocks of the book (`mdbook test`) against a freshly built graphersal.
# A dedicated target dir holds exactly one graphersal rlib, so the library path is unambiguous.
# Usage: book/test.sh   (env: BOOK_TEST_TARGET_DIR, default target/mdbook-test, git-ignored)
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
dir=${BOOK_TEST_TARGET_DIR:-$root/target/mdbook-test}
cd "$root"
CARGO_TARGET_DIR=$dir cargo build -p graphersal --features script,io,display,serde,persist,gql
deps=$dir/debug/deps
n=$(ls "$deps"/libgraphersal-*.rlib | wc -l)
if [ "$n" -ne 1 ]; then echo "expected exactly one libgraphersal rlib in $deps, found $n" >&2; exit 1; fi
rlib=$(ls "$deps"/libgraphersal-*.rlib)
# mdbook has no --extern option: a PATH shim passes it to rustdoc so `use graphersal::...` resolves.
shim=$dir/shim
mkdir -p "$shim"
real=$(command -v rustdoc)
printf '#!/bin/sh\nexec "%s" "$@" --extern "graphersal=%s"\n' "$real" "$rlib" > "$shim/rustdoc"
chmod +x "$shim/rustdoc"
PATH=$shim:$PATH exec mdbook test book -L "$deps"
