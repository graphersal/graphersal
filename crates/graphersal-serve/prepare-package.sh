#!/bin/sh
# Copies the playground entries the crate embeds (build.rs `ENTRIES`, Cargo.toml `include`) into
# ui/ next to this script, so `cargo package -p graphersal-serve` / `cargo publish` carry them.
# ui/ is git-ignored; a checkout builds from ../../playground and never reads it.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src="$here/../../playground"
rm -rf "$here/ui"
mkdir -p "$here/ui"
for entry in index.html css js assets reset; do
  cp -R "$src/$entry" "$here/ui/$entry"
done
find "$here/ui" -name '.*' -exec rm -rf {} +
echo "copied the playground UI into $here/ui"
