"""`graphersal.Store`: one graph in one directory, commits durable, reopen, checkpoint, marks,
verify, fork, backup."""

import pathlib
import shutil

import pytest

import graphersal


def names(graph):
    return sorted(graph.query('g.V().values("name").toList()'))


def test_a_store_keeps_commits_across_reopen(tmp_path):
    path = str(tmp_path / "data")
    with graphersal.Store.create(path) as store:
        graph = store.graph
        graph.execute('g.addV("person").property("name", "ann").next()')
        store.mark("ann")
        snapshot = store.checkpoint("first")
        assert snapshot["commit_seq"] == 1 and snapshot["name"] == "first"
        graph.execute('g.addV("person").property("name", "bob").next()')
        with pytest.raises(graphersal.GraphersalError):
            graphersal.Store.open(path)  # one writer
    # Closed: the graph refuses commits.
    with pytest.raises(graphersal.GraphersalError):
        graph.execute('g.addV("person").next()')
    store = graphersal.Store.open(path)
    assert store.open_report()["clean_close"]
    assert names(store.graph) == ["ann", "bob"]
    assert [s["commit_seq"] for s in store.snapshots()] == [0, 1]
    assert [m["name"] for m in store.marks()] == ["ann"]
    assert store.verify()["ok"]
    fork = store.fork(str(tmp_path / "fork"), mark="ann")
    assert fork["commit_seq"] == 1
    assert fork["marks_not_carried"] == [{"name": "ann", "commit_seq": 1}]
    store.backup(str(tmp_path / "backup"))
    store.close()
    store.close()  # twice is fine
    with graphersal.Store.open(str(tmp_path / "fork")) as forked:
        assert names(forked.graph) == ["ann"]
    # A backup opens read-only until it is restored.
    with pytest.raises(Exception, match="is a backup"):
        graphersal.Store.open(str(tmp_path / "backup"))
    with graphersal.Store.open_backup(str(tmp_path / "backup")) as copy:
        assert names(copy.graph) == ["ann", "bob"]
        assert copy.backup_marker["commit_seq"] == 2
    assert graphersal.Store.restore_backup(str(tmp_path / "backup"))["commit_seq"] == 2
    with graphersal.Store.open(str(tmp_path / "backup")) as copy:
        assert names(copy.graph) == ["ann", "bob"]
        assert copy.backup_marker is None


def test_incremental_backups(tmp_path):
    backup = str(tmp_path / "backup")
    with graphersal.Store.create(str(tmp_path / "data")) as store:
        store.graph.execute('g.addV("person").property("name", "ann").next()')
        assert not store.backup(backup)["incremental"]
        store.graph.execute('g.addV("person").property("name", "bob").next()')
        report = store.backup(backup)
        assert report["incremental"] and report["previous_commit_seq"] == 1
        assert report["commit_seq"] == 2 and report["tail_bytes"] > 0
        with pytest.raises(Exception, match="not empty"):
            store.backup(backup, full=True)
    with graphersal.Store.open_backup(backup) as copy:
        assert names(copy.graph) == ["ann", "bob"]
        with pytest.raises(Exception, match="backup"):
            copy.graph.execute('g.addV("x").next()')


def test_rollback_attic_restore_and_repair(tmp_path):
    path = str(tmp_path / "data")
    with graphersal.Store.create(path) as store:
        graph = store.graph
        graph.execute('g.addV("person").property("name", "ann").next()')
        store.mark("ann")
        graph.execute('g.addV("person").property("name", "bob").next()')
        assert store.maintenance is None
        with pytest.raises(ValueError):
            store.rollback()
        report = store.rollback(mark="ann")
        assert report["target"] == 1 and report["previous_commit_seq"] == 2
        assert names(store.graph) == ["ann"]
        entries = store.attic()
        assert [e["target"] for e in entries] == [1]
        assert store.attic_restore(entries[0]["id"]) == 2
        assert names(store.graph) == ["ann", "bob"]
        repaired = store.repair_to(str(tmp_path / "repaired"))
        assert repaired["lossless"] and repaired["commit_seq"] == 2
        assert repaired["marks_not_carried"] == [{"name": "ann", "commit_seq": 1}]
        assert repaired["lineage_approximate_before"] is None
        assert "mark not carried: ann @ commit 1" in repaired["report"]
    with graphersal.Store.open(str(tmp_path / "repaired")) as copy:
        assert names(copy.graph) == ["ann", "bob"]


def test_a_single_file_store_compacts_and_converts(tmp_path):
    path = str(tmp_path / "graph.gstore")
    with graphersal.Store.create(path) as store:
        for round in range(3):
            store.graph.execute("g.V().drop().toList()")
            store.graph.execute(f'g.addV("person").property("name", "p{round}").next()')
            store.checkpoint()
        advice = store.compaction_advice()
        assert not advice["recommended"] and advice["decided_by"] == "total_bytes"
        assert advice["compactable"] and advice["prune_first"]
        assert advice["marks_unreachable"] == []
        low = store.compaction_advice(min_ratio=0.0, min_reclaimable=1, min_size=0)
        assert low["recommended"], low
        with pytest.raises(ValueError):
            store.compaction_advice(min_ratio=2.0)
        done = store.compact()
        assert done["compacted"] and done["bytes_freed"] > 0
        assert done["snapshots_removed"]
        assert store.prune(0)["snapshots_removed"] == []
        assert store.prune(0)["marks_unreachable"] == []
        assert names(store.graph) == ["p2"]
    assert (tmp_path / "graph.gstore").is_file()
    copied = graphersal.Store.convert(path, str(tmp_path / "dir"))
    assert copied["files"] > 3 and copied["bytes"] > 0
    graphersal.Store.convert(str(tmp_path / "dir"), str(tmp_path / "plain"), single_file=True)
    with graphersal.Store.open_file(str(tmp_path / "plain")) as store:
        assert names(store.graph) == ["p2"]
    with graphersal.Store.create_file(str(tmp_path / "other.bin")) as store:
        store.graph.execute('g.addV("person").property("name", "x").next()')
    with graphersal.Store.open(str(tmp_path / "other.bin")) as store:
        assert names(store.graph) == ["x"]


def damage_newest_wal(path):
    """Flips a byte in the last record of the store's newest WAL segment (a failing disk)."""
    import pathlib

    newest = sorted(pathlib.Path(path, "wal").glob("*.wal"))[-1]
    data = bytearray(newest.read_bytes())
    data[-5] ^= 0x40
    newest.write_bytes(bytes(data))


@pytest.mark.parametrize("policy", ["maintenance", "continue"])
def test_damage_policy_and_a_backup_from_memory(tmp_path, policy):
    path = str(tmp_path / "data")
    with pytest.raises(ValueError, match="damage policy"):
        graphersal.Store.create(str(tmp_path / "bad"), on_damage="sometimes")
    with graphersal.Store.create(path, on_damage=policy) as store:
        assert store.damage_policy == policy
        assert store.damage_found == [] and not store.frozen_by_damage
        store.graph.execute('g.addV("person").property("name", "ann").next()')
        # Only for a damaged store.
        with pytest.raises(Exception, match="only for a store whose files are damaged"):
            store.backup(str(tmp_path / "memory"), from_memory=True)
        damage_newest_wal(path)
        with pytest.raises(graphersal.GraphersalError, match="is damaged") as damaged:
            store.backup(str(tmp_path / "backup"))
        assert "store repair" in damaged.value.help and "from memory" in damaged.value.help
        assert not (tmp_path / "backup").exists()
        assert store.damage_found[0]["found_by"] == "backup"
        assert store.frozen_by_damage == (policy == "maintenance")
        if policy == "maintenance":
            with pytest.raises(Exception, match="read-only"):
                store.graph.execute('g.addV("person").property("name", "bob").next()')
        else:
            store.graph.execute('g.addV("person").property("name", "bob").next()')
        expected = names(store.graph)
        report = store.backup(str(tmp_path / "memory"), from_memory=True)
        assert report["from_memory"] and not report["incremental"]
    graphersal.Store.restore_backup(str(tmp_path / "memory"))
    with graphersal.Store.open(str(tmp_path / "memory")) as saved:
        assert names(saved.graph) == expected
        assert saved.damage_policy == policy  # carried by the backup


def test_a_fork_has_its_own_store_id_and_cannot_continue_a_backup(tmp_path):
    with graphersal.Store.create(str(tmp_path / "data")) as store:
        store.graph.execute('g.addV("person").property("name", "ann").next()')
        original = store.store_id
        assert len(original) == 36
        store.backup(str(tmp_path / "backup"))
        store.fork(str(tmp_path / "fork"))
    with graphersal.Store.open(str(tmp_path / "fork")) as fork:
        assert fork.store_id != original
        fork.graph.execute('g.addV("person").property("name", "bob").next()')
        with pytest.raises(graphersal.GraphersalError, match="different store"):
            fork.backup(str(tmp_path / "backup"))


def test_a_refused_store_operation_names_its_cause(tmp_path):
    with graphersal.Store.create(str(tmp_path / "data")) as store:
        store.graph.execute('g.addV("person").property("name", "ann").next()')
        store.checkpoint("weekly")
        store.graph.execute('g.addV("person").property("name", "bob").next()')
        with pytest.raises(graphersal.GraphersalError, match="already exists") as taken:
            store.checkpoint("weekly")
        assert taken.value.cause == "name_taken"
        assert "Choose another name" in taken.value.help
    with pytest.raises(graphersal.GraphersalError) as closed:
        store.checkpoint()
    assert closed.value.cause == "closed"
    # Every other error has no cause.
    with pytest.raises(graphersal.GraphersalError) as other:
        graphersal.Store.open(str(tmp_path / "missing"))
    assert other.value.cause is None


FIXTURES = pathlib.Path(__file__).resolve().parents[3] / "crates/graphersal/tests/data/v0.1.0"


def test_a_store_of_graphersal_0_1_is_upgraded(tmp_path):
    old = tmp_path / "old"
    shutil.copytree(FIXTURES / "store", old)
    with pytest.raises(graphersal.GraphersalError) as refused:
        graphersal.Store.open(str(old))
    assert "graphersal store upgrade" in refused.value.help
    with pytest.raises(ValueError):
        graphersal.Store.upgrade(str(old))
    report = graphersal.Store.upgrade(str(old), str(tmp_path / "new"))
    assert report["from_version"] == 2 and not report["in_place"]
    assert report["files"] > 10 and report["backup"] is None
    with graphersal.Store.open(str(tmp_path / "new")) as store:
        assert "grace" in names(store.graph)
        assert [m["name"] for m in store.marks()] == ["first", "before-rollback", "last"]
    # In place, then nothing to do; a copy of a current store is refused with its cause.
    assert graphersal.Store.upgrade(str(old), in_place=True)["in_place"]
    assert graphersal.Store.upgrade(str(old), in_place=True)["already_current"]
    with pytest.raises(graphersal.GraphersalError) as current:
        graphersal.Store.upgrade(str(old), str(tmp_path / "again"))
    assert current.value.cause == "already_current"
    # A backup stays a backup.
    shutil.copytree(FIXTURES / "backup", tmp_path / "old_backup")
    backup = graphersal.Store.upgrade(str(tmp_path / "old_backup"), str(tmp_path / "backup"))
    assert backup["backup"]["commit_seq"] == 7


def test_temporal_values_survive_the_store_exactly(tmp_path):
    """File format version 3: a zoned datetime (a ZoneInfo), an offset time, nanoseconds and the
    longest durations are stored and read back exactly, through the WAL and a checkpoint."""
    import datetime as dt
    import zoneinfo

    berlin = zoneinfo.ZoneInfo("Europe/Berlin")
    zoned = dt.datetime(2024, 3, 30, 12, 0, 0, 123456, tzinfo=berlin)
    offset_time = dt.time(10, 0, tzinfo=dt.timezone(dt.timedelta(hours=5, minutes=45)))
    path = str(tmp_path / "data")
    texts = {
        "ns": 'time("12:00:00.000000001")',
        "ldt": 'localdatetime("9999-12-31T23:59:59.999999999")',
        "long": 'duration("PT9223372036854775807.999999999S")',
        "short": 'duration("-PT9223372036854775808S")',
    }
    with graphersal.Store.create(path) as store:
        store.graph.execute(
            'g.addV("e").property("name", "x").property("zoned", z).property("ot", o).iterate()',
            params={"z": zoned, "o": offset_time},
        )
        for key, value in texts.items():
            store.graph.execute(f'g.V().has("name", "x").property("{key}", {value}).iterate()')

    def check(graph):
        got = graph.query('g.V().has("name", "x").values("zoned").next()')
        assert got == zoned and got.tzinfo == berlin
        assert graph.query('g.V().has("name", "x").values("ot").next()') == offset_time
        for key, value in texts.items():
            assert graph.query(
                f'g.V().has("name", "x").values("{key}").as_string().next()'
            ) == graph.query(f"g.inject({value}).as_string().next()")

    with graphersal.Store.open(path) as store:
        check(store.graph)  # WAL replay
        store.checkpoint("temporal")
    with graphersal.Store.open(path) as store:
        check(store.graph)  # from the snapshot
        assert store.verify()["ok"]


def test_the_workload_report_counts_python_queries(tmp_path):
    with graphersal.Store.create(str(tmp_path / "data")) as store:
        store.graph.execute('g.addV("person").property("name", "ann").next()')
        assert names(store.graph) == ["ann"]
        report = store.workload_report()
        assert report["level"] == "persisted"
        channels = {row["channel"] for row in report["queries"]}
        assert channels == {"python"}
        assert any(row["key"] == "name" and row["access"] == "write" for row in report["properties"])
        assert all(row["last_hour"]["count"] >= 1 for row in report["queries"])
