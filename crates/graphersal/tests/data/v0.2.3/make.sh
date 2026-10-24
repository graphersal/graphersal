#!/bin/sh
# Regenerates the golden fixtures of Graphersal 0.2.3 (store format version 3, file format
# version 2: the microsecond temporal value tags 9 to 13) with a RELEASED 0.2.3 binary, for the
# file format version 3 tests (crates/graphersal/tests/all/temporal_v3_format_tests.rs). The
# tests pin the values written below, so regenerate them only on purpose.
#
#   usage: make.sh <path to the graphersal 0.2.3 binary> [<output dir, default: this dir>]
#
# Build the binary from the release tag in a separate checkout, for example:
#   git worktree add /tmp/v023 v0.2.3 && (cd /tmp/v023 && cargo build -p graphersal-cli)
#   -> /tmp/v023/target/debug/graphersal
set -eu
BIN=${1:?usage: make.sh <graphersal 0.2.3 binary> [<out dir>]}
OUT=${2:-$(cd "$(dirname "$0")" && pwd)}
"$BIN" --version | grep -q '^graphersal 0\.2\.3$' || { echo "not a 0.2.3 binary: $BIN" >&2; exit 2; }
cd "$OUT"
rm -rf store backup.zip snapshot.gsnap
run() { "$BIN" --graph "$1" -e "$2" > /dev/null; }

# Every temporal kind of file format version 2 at its range edges (tags 9 to 13), plain and
# nested in a list and an object.
EDGES='property("d_min", date("-9999-01-01")).property("d_max", date("9999-12-31"))
  .property("ldt_min", localdatetime("-9999-01-01T00:00:00")).property("ldt_max", localdatetime("9999-12-31T23:59:59.999999"))
  .property("dt_min", datetime("-9999-01-01T00:00:00-18:00")).property("dt_max", datetime("9999-12-31T23:59:59.999999+18:00"))
  .property("dt", datetime("2024-03-31T02:30:00.123456+02:00"))
  .property("t_min", time("00:00:00")).property("t_max", time("23:59:59.999999"))
  .property("dur", duration("P1Y2M3DT4H5M6.789012S")).property("dur_neg", duration("-PT0.000001S"))
  .property("dur_max", duration("PT9223372036854.775807S")).property("dur_min", duration("-PT9223372036854.775808S"))
  .property("dur_cal", duration("P2147483647M-2147483648D"))
  .property("nested", [date("2024-01-01"), #{at: time("12:00:00.5"), when: datetime("2024-01-01T00:00:00Z")}])'

# A directory store: commit 1 (vertex `snap` with every edge value) in the snapshot of the merged
# checkpoint `base`; after it, in the WAL (version 2): commit 2 (vertex `wal` with the same
# values), commit 3 (`snap`'s `dt` changed: a SetProperty with a temporal before image), commit 4
# (an edge `at` from `wal` to `snap` with a temporal property), the mark `last` (4).
"$BIN" store create store > /dev/null
run store "g.add_v(\"thing\").property(\"name\", \"snap\").$EDGES.iterate()"
"$BIN" store checkpoint store --name base > /dev/null
run store "g.add_v(\"thing\").property(\"name\", \"wal\").$EDGES.iterate()"
run store 'g.v().has("name", "snap").property("dt", datetime("2000-02-29T23:59:59.000001-05:30")).iterate()'
run store 'g.v().has("name", "wal").add_e("at").to(__.v().has("name", "snap")).property("when", localdatetime("1969-12-31T23:59:59.999999")).iterate()'
"$BIN" store mark store last > /dev/null

# A packed snapshot of the same graph and a ZIP backup of the store.
run store 'g.export_snapshot("snapshot.gsnap")'
"$BIN" store backup store backup.zip --zip > /dev/null

# The lock files carry no data (an open recreates them).
rm -f store/LOCK store/BACKUP
echo "fixtures written to $OUT"
