# Golden fixtures of Graphersal 0.2.3

Files written by the RELEASED Graphersal 0.2.3 binary (store format version 3, file format
version 2: temporal values in the microsecond tags 9 to 13), for the file format version 3 tests
(`tests/all/temporal_v3_format_tests.rs`). Every later build must keep reading them exactly and
continue the store in its own file format version. The tests copy them first: these files never
change.

| Fixture | What it holds |
|---|---|
| `store/` | A directory store: commit 1 (vertex `snap` with every temporal kind at its range edges, plain and nested) in the snapshot of the merged checkpoint `base`; WAL (one version 2 segment) with commit 2 (vertex `wal`, the same values), commit 3 (`snap`'s `dt` changed to `2000-02-29T23:59:59.000001-05:30`), commit 4 (edge `at` from `wal` to `snap`, `when` = `localdatetime 1969-12-31T23:59:59.999999`), the mark `last` (4). |
| `snapshot.gsnap` | A packed snapshot of the same graph state (commit 4). |
| `backup.zip` | A ZIP backup of `store/`. |

They were produced by `make.sh` with the binary built from the tag `v0.2.3`
(`git worktree add <dir> v0.2.3`, `cargo build -p graphersal-cli` there):

```sh
crates/graphersal/tests/data/v0.2.3/make.sh <dir>/target/debug/graphersal
```

The script lists every command and value. The lock files (`LOCK`, `BACKUP`) are removed
afterwards; they hold no data. Regenerating changes ids and times, which the tests do not pin.
