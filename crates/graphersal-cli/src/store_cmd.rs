//! `graphersal store <command> ...`: manage a Store directory (the persistence of one graph;
//! reference: the book page `book/src/persistence/store_cli.md`). Commands that only read
//! (`info`, `list`, `marks`, `verify`, `backup`, `export`, `fork`) work while a writer (the dev
//! server, a REPL) has the store open; the others open it read-write and fail with "in use"
//! then. A backup directory (marked in its `GRAPH` file) is read-only: the reading commands and
//! `prune` work on it, `restore <DIR>` makes it the live store.

use std::path::Path;

use std::sync::Arc;

use graphersal::persist::{
    BackupMode, BackupReport, CompactionAdvice, CompactionPolicy, DamagePolicy, ModelCopy,
    PersistError, RecoveryTarget, RollbackMode, SingleFileDir, Store, StoreDir, StoreOptions,
    StoredModel, store_dir_at,
};

use crate::cli::{EXIT_OK, EXIT_QUERY_ERROR, EXIT_USAGE_ERROR, GraphSpec};
pub use graphersal_session::store::{bytes_text, format_time, parse_time, uuid_text};

/// Usage of `graphersal store`.
pub const STORE_HELP: &str = r#"graphersal store - manage a Store (one directory, or one .gstore file = one graph)

USAGE:
    graphersal store create <DIR> [--from <SOURCE>] [--chunk-size <BYTES>] [--single-file]
                                     [--on-damage maintenance|continue]
                                     create a store; --from takes the --graph sources (modern,
                                     empty, large, tree, a GraphSON/GraphML file, a .gsnap);
                                     DIR ending in .gstore (or --single-file): ONE file;
                                     --chunk-size and --on-damage are fixed for its life
    graphersal store convert <DIR> <NEW> [--single-file]
                                     copy a closed store into a new directory or a single file
    graphersal store upgrade <DIR> <NEW> | <DIR> --in-place
                                     convert a store (or backup) of an older Graphersal to the
                                     current format, into NEW or in place; no older binary needed
    graphersal store compact <DIR> [--drop-marks]
                                     prune, then give a single file's free space back;
                                     refused when marks would become unreachable, unless
                                     --drop-marks
    graphersal store compact-advice <DIR> [--min-ratio <R>] [--min-reclaimable <BYTES>]
                                     [--min-size <BYTES>]
                                     whether `compact` is worth it now, and the numbers
    graphersal store info <DIR>      identity, position, snapshots, marks, WAL size
    graphersal store list <DIR>      the snapshots (commit, time, counts, name)
    graphersal store marks <DIR>     the marks
    graphersal store mark <DIR> <NAME>              set a mark at the current position
    graphersal store checkpoint <DIR> [--name <N>]  merge a snapshot of the current state
    graphersal store verify <DIR>    scrub every checksum; exit 1 when damage is found
    graphersal store prune <DIR> --up-to <COMMIT> [--drop-marks]
                                     remove snapshots and WAL only needed for targets before
                                     COMMIT (the last two snapshots always stay); refused when
                                     marks would become unreachable, unless --drop-marks
    graphersal store fork <DIR> <NEW_DIR> [TARGET]  a new store from the state at TARGET
    graphersal store backup <DIR> <BACKUP_DIR> [--full]
                                     a consistent copy (works while in use): full into an empty
                                     directory, INCREMENTAL into an existing backup of DIR;
                                     every checksum is read in DIR and again in the copy
    graphersal store backup <DIR> <FILE.zip> --zip [--without-models]
                                     a full backup as one ZIP archive (--without-models: the
                                     embedding models left out)
    graphersal store restore <BACKUP_DIR>           make a backup the live store, in place
    graphersal store restore <FILE.zip> <DIR>       unpack a ZIP backup into a new directory
    graphersal store export <DIR> <FILE.gsnap> [--commit <N>] [--without-models]
                                     the latest (or the given) stored snapshot as a packed file,
                                     with the store's embedding models unless --without-models
    graphersal store models <DIR>    the embedding models the store keeps, with their licences
    graphersal store models remove <DIR> <IDENTITY>
                                     remove a model (by identity or id) from the store
    graphersal store rollback <DIR> TARGET [--delete]
                                     roll the store back IN PLACE: everything after TARGET moves
                                     to the attic (--delete: deleted); a new lineage continues
    graphersal store attic <DIR>                    the attic entries (rolled-back history)
    graphersal store attic <DIR> changes <ID>       the commits of an attic entry
    graphersal store attic <DIR> restore <ID>       undo its rollback (only while nothing was
                                     committed since)
    graphersal store attic <DIR> fork <ID> <NEW_DIR>  a new store from the end of the entry
    graphersal store attic <DIR> remove <ID>        delete an attic entry for good
    graphersal store repair <DIR> --to <NEW_DIR>    build a repaired copy of a damaged store
                                     in a NEW directory (the damaged files are never changed)

TARGET: --at-commit <N> | --at-time <TIME> | --at-mark <NAME> (fork: default the latest).
TIME: an RFC 3339 / ISO 8601 date-time with a zone (2026-10-08T02:43:00Z, 2026-10-08T02:43Z,
2026-10-08T04:43:00+02:00) or microseconds since the Unix epoch; it covers the whole second
(minute) it names, so a time copied from `store list` includes that commit. A bare date is
refused: write 2026-10-08T23:59:59Z (the end of the day in UTC) or 2026-10-08T00:00:00+02:00.
DIR: a store directory, or a single-file store (an existing file, or a new path ending in
.gstore): every command works on both.
A damaged store opens read-only in maintenance mode; `verify` lists the damage and its donors.
A backup directory is read-only (info, list, marks, verify, export, fork, prune work on it;
--graph opens it read-only) until `store restore <BACKUP_DIR>` makes it the live store.
Embedding models (models/ in the store) enter it only by `graphersal embed-model download
<REPO> --into <DIR>` or `embed-model import <MODEL_DIR> --into <DIR>`; every copy (backup,
fork, convert, repair, export) carries them with their licences: a store handed on
redistributes its models. The store's models are loaded when the store is opened.

Snapshots and marks: a snapshot is a full copy of the graph at a commit (a fast start, a
`.gsnap` download); a mark is a name for a position in the history. Every commit can be a
target, not only the snapshots and marks.

`graphersal store <COMMAND> --help` (or `store help <COMMAND>`) explains one command.
Use the store: graphersal --graph <DIR> (REPL, -e, --server). Exit codes: 0 ok, 1 the
operation failed (or verify found damage), 2 invalid invocation. Store warnings go to
stderr; GRAPHERSAL_LOG=off|error|warn|info|debug|trace sets the level (default warn).
"#;

/// The target options, as every command that takes one explains them.
const TARGET_HELP: &str = "TARGET (one of):
    --at-commit <N>      right after commit N
    --at-time <TIME>     after the last commit at or before TIME: an RFC 3339 / ISO 8601
                         date-time with a zone (2026-10-08T02:43:00Z, 2026-10-08T02:43Z,
                         2026-10-08T04:43:00+02:00) or microseconds since the Unix epoch; a
                         time covers the whole second (minute) it names; a bare date is
                         refused (2026-10-08T23:59:59Z = the end of that day in UTC)
    --at-mark <NAME>     at the mark NAME (graphersal store marks <DIR> lists them)
";

/// The usage of one `graphersal store` command (`store <COMMAND> --help`, `store help
/// <COMMAND>`), or `None` for an unknown command.
pub fn command_help(command: &str) -> Option<String> {
    let (usage, text, target): (&str, &str, bool) = match command {
        "create" => (
            "graphersal store create <DIR> [--from <SOURCE>] [--chunk-size <BYTES>] [--single-file] [--on-damage maintenance|continue]",
            "Creates a store in the new directory DIR. --from takes the --graph sources: modern,\n\
             empty, large, tree, a GraphSON/GraphML file or a packed snapshot (.gsnap); default:\n\
             an empty graph. --chunk-size sets the snapshot chunk size (default 1 MiB).\n\
             A DIR ending in .gstore, or --single-file, makes a SINGLE-FILE store: the whole\n\
             store in that one file (embedded use; move or copy it while it is closed). Every\n\
             command works on it as on a directory; `store compact` gives its free space back.\n\
             --on-damage sets what the store does when damage is found on disk while it is open\n\
             (by a backup, a checkpoint, a verification): \"maintenance\" (the default) makes it\n\
             read-only, \"continue\" keeps it committing and reports the damage. Damage found\n\
             when it is opened always means maintenance mode. The chunk size, the damage policy\n\
             and the backend are fixed for the store's life: forks, repairs, conversions and\n\
             backups carry them, nothing changes them (`store info` shows them).",
            false,
        ),
        "convert" => (
            "graphersal store convert <DIR> <NEW> [--single-file]",
            "Copies the whole store DIR (snapshots, WAL, marks, attic) into NEW, byte for byte:\n\
             a directory into a single file (NEW ending in .gstore, or --single-file) or back.\n\
             NEW must not exist (or be empty); DIR must be closed (no server on it). The copy\n\
             is verified; DIR is not changed.",
            false,
        ),
        "upgrade" => (
            "graphersal store upgrade <DIR> <NEW> [--single-file] | graphersal store upgrade <DIR> --in-place",
            "Converts a store written by an older Graphersal (0.1.x) to the current format; no\n\
             older binary is needed. With NEW: a new store in NEW (a new directory, or a single\n\
             file: NEW ending in .gstore, or --single-file); DIR is not changed. With\n\
             --in-place: DIR itself (back it up first: afterwards the older Graphersal no longer\n\
             opens it). Everything is carried: the graph, schema, catalog, the history, marks,\n\
             attic entries and the store's identity and creation parameters; only GRAPH and\n\
             GRAPH.copy are rewritten, every other file is kept byte for byte. DIR may be a\n\
             directory, a .gstore file or a backup directory (it stays a backup). DIR is verified\n\
             first: a damaged store is refused (store repair DIR --to NEW writes a repaired\n\
             store of the current format), and so is a store in use (no server on it). The copy\n\
             is verified before it becomes a store; an interrupted upgrade leaves no store in\n\
             NEW (run it again). A ZIP backup of an older store is upgraded by `store restore`.",
            false,
        ),
        "compact" => (
            "graphersal store compact <DIR> [--drop-marks]",
            "Prunes the store up to its current commit (the last two snapshots, the WAL between\n\
             them and the attic's bases stay), then, for a single-file store, copies its live\n\
             data into a new file next to it and replaces the file (the disk needs room for the\n\
             live data meanwhile). For a directory the prune is all. Fails with \"in use\" while\n\
             a server has the store open (use its Store menu then). `store compact-advice` says\n\
             whether it is worth it. When the prune would make marks unreachable (their history\n\
             goes), it names them and changes nothing (exit 1) unless --drop-marks is given.",
            false,
        ),
        "compact-advice" => (
            "graphersal store compact-advice <DIR> [--min-ratio <R>] [--min-reclaimable <BYTES>] [--min-size <BYTES>]",
            "Says whether `store compact` is worth it now, and why: the file's size, its live\n\
             and garbage bytes, what a prune would free first, the free disk space a compaction\n\
             needs. Cheap (reads no element data) and works while a server has the store open. The rule:\n\
             at least --min-ratio of the store (default 0.5) and --min-reclaimable bytes (default\n\
             64 MiB) to gain, in a store of at least --min-size bytes (default 64 MiB), and\n\
             enough free disk space.",
            false,
        ),
        "info" => (
            "graphersal store info <DIR>",
            "Prints the store's identity (graph id, lineage), position (commit, time), snapshots,\n\
             marks, WAL size and attic, the embedding models it keeps (identity, size, licence,\n\
             whether the schema uses them; models the schema names but the store lacks), and\n\
             the parameters fixed when it was created (chunk size, damage policy, backend).\n\
             Works while a server has the store open.",
            false,
        ),
        "list" => (
            "graphersal store list <DIR>",
            "Lists the snapshots: commit, time of its last commit, counts, name. A snapshot is a\n\
             full copy of the graph at a commit (a fast start; `store export` writes one as a\n\
             .gsnap). Any commit can be a target, not only a snapshot's.",
            false,
        ),
        "marks" => (
            "graphersal store marks <DIR>",
            "Lists the marks: commit, time, name. A mark is a name for a position in the history\n\
             (--at-mark NAME targets it); it costs nothing and copies no data.",
            false,
        ),
        "mark" => (
            "graphersal store mark <DIR> <NAME>",
            "Sets the mark NAME at the store's current position. Names are unique along the\n\
             store's whole history. Fails with \"in use\" while a server has the store open (use\n\
             the server's Store menu then).",
            false,
        ),
        "checkpoint" => (
            "graphersal store checkpoint <DIR> [--name <NAME>]",
            "Writes a snapshot of the current state (shortens the replay at the next open):\n\
             merged from the last snapshot and the WAL after it, without loading the graph\n\
             (memory for the changes only; unchanged segment files are reused). The store must\n\
             be closed: fails with \"in use\" while a server has it open (use its Store menu,\n\
             which checkpoints the same way without stopping commits). Damage in what it reads\n\
             stops it with nothing written; run `graphersal store verify <DIR>` then.",
            false,
        ),
        "verify" => (
            "graphersal store verify <DIR>",
            "Reads every checksum of the store (snapshots, WAL, GRAPH, marks). Exit 1 and a damage\n\
             report with the donor of each damaged item when damage is found.",
            false,
        ),
        "prune" => (
            "graphersal store prune <DIR> --up-to <COMMIT> [--drop-marks]",
            "Removes snapshots and WAL only needed for targets before COMMIT. The last two\n\
             snapshots and the base of every attic entry always stay; nothing is pruned without\n\
             --up-to. Works on a backup directory too (a backup has its own retention). The\n\
             WAL is not kept for backups: an incremental backup that needs pruned WAL is\n\
             refused (back up into a new directory with --full then). When the prune would make\n\
             marks unreachable (their history goes), it names them and changes nothing (exit 1)\n\
             unless --drop-marks is given.",
            false,
        ),
        "fork" => (
            "graphersal store fork <DIR> <NEW_DIR> [TARGET]",
            "Makes a NEW store in NEW_DIR (it must not exist) from the state at TARGET (default:\n\
             the latest), with a new lineage that records DIR as its parent. DIR is not changed;\n\
             to undo a fork, delete NEW_DIR. Works while a server has DIR open.",
            true,
        ),
        "backup" => (
            "graphersal store backup <DIR> <BACKUP_DIR> [--full]\n    graphersal store backup <DIR> <FILE.zip> --zip",
            "A consistent copy of the store up to the last durable commit; works while a server\n\
             has the store open. Into an empty or new BACKUP_DIR: a FULL backup (the latest\n\
             snapshot and the WAL after it). Into an existing backup of DIR: INCREMENTAL (only\n\
             the new snapshots, the new WAL segments and the new end of the last one are\n\
             copied, after checking that the backup's WAL is a prefix of the store's; an\n\
             interrupted increment is rolled back at the next run). Refused for a backup of\n\
             another graph, for a backup the store was rolled back behind, and when the store\n\
             pruned WAL the backup still needs: back up into a new directory with --full then.\n\
             --full forces a full backup (BACKUP_DIR must be empty or new). The backup is\n\
             marked: it opens read-only until `store restore BACKUP_DIR`; any commit it holds is\n\
             a target (--at-commit, ...) of `fork` and of a read-only view. `store prune` works\n\
             on it. --zip writes one ZIP archive instead (always full; `store restore\n\
             FILE.zip DIR` unpacks it as the live store).\n\
             The store's embedding models (with their cards and licences) are copied too and\n\
             listed in the report: who hands a backup on redistributes them. --without-models\n\
             (with --zip only) leaves them out: such an archive is also restored by Graphersal\n\
             0.3.0, which refuses one with models as written by a newer version.\n\
             Every checksum of what is copied is read in DIR first (GRAPH, marks, the snapshot,\n\
             every WAL record) and again in the copy (an archive is read back). Damage stops the\n\
             backup: nothing of it is kept (an increment is rolled back) and the report names\n\
             the damaged files; repair the store then (`store repair DIR --to NEW_DIR`), never\n\
             the backup. Damage of a redundant copy (one GRAPH copy, one manifest copy, the marks\n\
             file) is a warning: the backup is intact.\n\
             --from-memory (a damaged store open in a server or a program): only where the store\n\
             is open in that process, through the dev server's Store menu (Back up from\n\
             memory), POST /api/store/backup {\"from_memory\": true}, or Python\n\
             store.backup(path, from_memory=True).",
            false,
        ),
        "restore" => (
            "graphersal store restore <BACKUP_DIR>\n    graphersal store restore <FILE.zip> <DIR>",
            "With one argument: makes the backup in BACKUP_DIR the LIVE store, in place (the\n\
             original is lost and you continue from the backup): its backup marker is cleared,\n\
             it keeps the original's graph id, and opens read-write from then on. To keep the\n\
             backup as it is, fork it instead (`store fork BACKUP_DIR NEW_DIR`).\n\
             With two: unpacks a ZIP backup (`store backup --zip`) into the new directory DIR\n\
             as the live store.",
            false,
        ),
        "export" => (
            "graphersal store export <DIR> <FILE.gsnap> [--commit <N>] [--without-models]",
            "Writes the latest stored snapshot (or the one at commit N) as one packed file;\n\
             load it with --graph FILE.gsnap or the playground's start page. The store's\n\
             embedding models travel with it (with their cards and licences; listed in the\n\
             report): who hands the file on redistributes them. --without-models leaves them\n\
             out; the receiver adds them again by the identities the schema records\n\
             (`graphersal embed-model download <REPO> --into <DIR>`). `store create <DIR>\n\
             --from FILE.gsnap` puts the models back into a store; Graphersal 0.3.0 loads such a\n\
             file and ignores its models.",
            false,
        ),
        "models" => (
            "graphersal store models <DIR>\n    graphersal store models remove <DIR> <IDENTITY>",
            "Lists the embedding models the store keeps (models/<id>/, format spec 4.5): the\n\
             identity a schema's `embedder` records (model2vec/<repo>@<12 hex>), the size, the\n\
             licence and where its text comes from (file: a licence file of the repository;\n\
             template: the standard text of the licence the model card names, written by\n\
             Graphersal; none), whether the repository is gated, and whether the schema uses\n\
             it. `remove` deletes one (by identity or by its id model2vec-<12 hex>): the only\n\
             way a model leaves a store; refused while a writer has the store open. A model\n\
             enters a store by `graphersal embed-model download <REPO> --into <DIR>` or\n\
             `embed-model import <MODEL_DIR> --into <DIR>`.",
            false,
        ),
        "rollback" => (
            "graphersal store rollback <DIR> TARGET [--delete]",
            "Rolls the store back IN PLACE to TARGET: everything after it (later snapshots and\n\
             WAL) moves to the attic, and the store continues from TARGET as a new lineage. Undo\n\
             it with `store attic <DIR> restore <ID>` as long as nothing new was committed or\n\
             marked; after that, `store attic <DIR> fork <ID> <NEW_DIR>` keeps the rolled-back\n\
             history as a new store. --delete deletes the history instead (no undo). Fails with\n\
             \"in use\" while a server has the store open (use its Store menu then).",
            true,
        ),
        "attic" => (
            "graphersal store attic <DIR>                       list the entries\n    \
             graphersal store attic <DIR> changes <ID>          the commits of an entry\n    \
             graphersal store attic <DIR> restore <ID>          undo its rollback\n    \
             graphersal store attic <DIR> fork <ID> <NEW_DIR>   the entry as a new store\n    \
             graphersal store attic <DIR> remove <ID>           delete the entry for good",
            "The attic keeps the history a rollback moved aside (one entry per rollback). A\n\
             restore is possible only while nothing was committed or marked since that\n\
             rollback; a fork always is.",
            false,
        ),
        "repair" => (
            "graphersal store repair <DIR> --to <NEW_DIR>",
            "Builds a repaired copy of a damaged store in NEW_DIR (never in place; the damaged\n\
             files are not changed) and reports what was repaired, lost and diverged. Exit 1 when\n\
             data is lost.",
            false,
        ),
        "help" => (
            "graphersal store help [<COMMAND>]",
            "Explains the store commands, or one of them.",
            false,
        ),
        _ => return None,
    };
    let mut out = format!("USAGE:\n    {usage}\n\n{text}\n");
    if target {
        out.push('\n');
        out.push_str(TARGET_HELP);
    }
    Some(out)
}

/// What a backup copied, as `store backup` prints it.
pub fn backup_summary(report: &BackupReport, target: &str) -> String {
    let list = |items: &[u64]| {
        if items.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = items.iter().map(u64::to_string).collect();
            format!(" [{}]", items.join(", "))
        }
    };
    let mut out = String::new();
    if !report.incremental {
        out.push_str(&format!(
            "Full backup into {target}: snapshot {}, the WAL up to commit {} ({} segment(s)); {} \
             file(s), {} bytes.\n",
            report.snapshot_commit_seq,
            report.commit_seq,
            report.segments_copied.len(),
            report.files.len(),
            report.bytes
        ));
    } else if report.bytes == 0 && !report.lineage_changed {
        out.push_str(&format!(
            "Incremental backup into {target}: nothing new since commit {}; the backup is \
             current.\n",
            report.commit_seq
        ));
    } else {
        out.push_str(&format!(
            "Incremental backup into {target}: commits {}..{} ({} new); copied {} snapshot(s){}, \
             {} WAL segment(s){}, {} bytes appended to the last segment; {} bytes in total.\n",
            report.previous_commit_seq + 1,
            report.commit_seq,
            report.commit_seq.saturating_sub(report.previous_commit_seq),
            report.snapshots_copied.len(),
            list(&report.snapshots_copied),
            report.segments_copied.len(),
            list(&report.segments_copied),
            report.tail_bytes,
            report.bytes
        ));
        if report.lineage_changed {
            out.push_str(
                "The store was rolled back in place since the last backup; the backup follows \
                 its new lineage.\n",
            );
        }
    }
    out.push_str(&models_carried(&report.models));
    out.push_str(&format!(
        "The backup is read-only (graphersal --graph {target} opens it so); `graphersal store \
         restore {target}` makes it the live store.\n"
    ));
    out
}

/// The lines naming the embedding models a copy carries, with their licences (empty without
/// models): who hands the copy on redistributes them.
pub fn models_carried(models: &[StoredModel]) -> String {
    if models.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "It carries {} embedding model(s) with their licences (handing it on redistributes \
         them):\n",
        models.len()
    );
    for model in models {
        out.push_str(&format!("  {}\n", model_line(model)));
    }
    out
}

/// Whether the schema's identity `named` (`model2vec/<repo>@<12 hex>`, possibly with a
/// `?normalize=` suffix) names the stored model `model` (its id `model2vec-<12 hex>`: the same
/// files, whatever repository name the schema recorded).
pub fn same_model(named: &str, model: &StoredModel) -> bool {
    let hex = named
        .split('?')
        .next()
        .and_then(|base| base.rsplit_once('@'))
        .map(|(_, hex)| hex);
    hex.is_some_and(|hex| model.id.strip_prefix("model2vec-") == Some(hex))
}

/// One model as the store commands show it: identity, size, licence and its text's source.
pub fn model_line(model: &StoredModel) -> String {
    let name = if model.identity.is_empty() {
        model.id.as_str()
    } else {
        model.identity.as_str()
    };
    let mut line = format!(
        "{name}  {}  licence {} ({})",
        bytes_text(model.bytes),
        model.license,
        match model.license_text.as_str() {
            "file" => "text from the repository",
            "template" => "standard text, written by Graphersal",
            _ => "no licence text",
        }
    );
    if model.gated {
        line.push_str(", gated repository");
    }
    if let Some(problem) = &model.problem {
        line.push_str(&format!(" [{problem}]"));
    }
    line
}

/// The backend for a path: a single file when asked (`--single-file`), else by the path
/// (an existing file or a new `.gstore` path is a single file, anything else a directory).
fn backend_of(path: &str, single_file: bool) -> Arc<dyn StoreDir> {
    if single_file {
        Arc::new(SingleFileDir::new(path))
    } else {
        store_dir_at(path)
    }
}

/// The compaction advice as `store compact-advice` prints it.
pub fn advice_text(advice: &CompactionAdvice, dir: &str) -> String {
    let mut out = format!(
        "{}: {}\n",
        if advice.recommended {
            "compact now"
        } else {
            "no compaction needed"
        },
        advice.reason
    );
    out.push_str(&format!(
        "store       {} ({})\n",
        bytes_text(advice.total_bytes),
        if advice.compactable {
            "single file"
        } else {
            "directory: compact = prune"
        }
    ));
    if advice.compactable {
        out.push_str(&format!(
            "live        {}\ngarbage     {} ({:.0} %)\n",
            bytes_text(advice.live_bytes),
            bytes_text(advice.garbage_bytes),
            advice.garbage_ratio * 100.0
        ));
    }
    out.push_str(&format!(
        "prune       {}{}\n",
        bytes_text(advice.prune_bytes),
        if advice.prune_first {
            format!(
                " first ({} snapshot(s), {} WAL segment(s))",
                advice.prune.snapshots_removed.len(),
                advice.prune.segments_removed.len()
            )
        } else {
            String::new()
        }
    ));
    out.push_str(&format!(
        "reclaimable {} ({:.0} % of the store)\n",
        bytes_text(advice.reclaimable_bytes),
        advice.reclaimable_ratio * 100.0
    ));
    if advice.compactable {
        out.push_str(&format!(
            "disk        {} needed, {}\n",
            bytes_text(advice.space_needed),
            advice
                .free_space
                .map_or("free space unknown".to_string(), |f| format!(
                    "{} free",
                    bytes_text(f)
                ))
        ));
    }
    let lost = &advice.prune.marks_unreachable;
    if !lost.is_empty() {
        out.push_str(&format!(
            "marks lost  {} (unreachable afterwards; compact asks for --drop-marks)\n",
            lost.join(", ")
        ));
    }
    out.push_str(&format!("decided by  {}\n", advice.decided_by.name()));
    if advice.recommended {
        let confirm = if lost.is_empty() { "" } else { " --drop-marks" };
        out.push_str(&format!("Run: graphersal store compact {dir}{confirm}\n"));
    }
    out
}

/// The stderr `note:` of an open that rewrote a missing, damaged or stale `GRAPH` /
/// `GRAPH.copy` from the other copy ([`graphersal::persist::OpenReport::graph_file_repaired`]),
/// or `None` when both copies were intact.
pub fn graph_file_repaired_note(path: &str, store: &Store) -> Option<String> {
    let read_only = store.backup_marker().is_some()
        || store.maintenance().is_some()
        || !matches!(store.read_only_reason(), Ok(None));
    store
        .open_report()
        .graph_file_repaired
        .as_ref()
        .map(|what| {
            if read_only {
                format!(
                    "note: the store {path}: {what}; the copy is left as it is (a read-only open \
                 writes nothing)"
                )
            } else {
                format!(
                    "note: the store {path}: {what}; the other copy was rewritten from it \
                 (`graphersal store verify {path}` checks the rest)"
                )
            }
        })
}

/// Opens a store read-write for a `store` command and prints the repaired-`GRAPH` note.
fn open_rw(path: &str) -> Result<Store, PersistError> {
    let store = Store::open(path, StoreOptions::new())?;
    if let Some(note) = graph_file_repaired_note(path, &store) {
        errln!("{note}");
    }
    Ok(store)
}

/// The refusal to create a store in `path` when it is an existing, non-empty directory
/// (`store create`, `--create-store`); `None` when the path is new, empty or not a directory.
pub fn create_refusal(path: &str) -> Option<String> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return None;
    }
    let is_store = match Store::is_store(path) {
        Ok(is_store) => is_store,
        // A directory whose files cannot be read: creating there would fail too.
        Err(err) => {
            return Some(format!(
                "Error: cannot create a store in {path}: {}",
                graphersal::error::format_diagnostic(&err)
            ));
        }
    };
    if is_store {
        return Some(format!(
            "Error: cannot create a store in {path}: it is a store already\nHelp: open it \
             (`graphersal --graph {path}`), see what it holds (`graphersal store info {path}`), \
             or choose an empty or new directory for the new store."
        ));
    }
    let empty = std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false);
    (!empty).then(|| {
        format!(
            "Error: cannot create a store in {path}: the directory is not empty\nHelp: choose an \
             empty or new directory, or open the existing store if it is one (`graphersal store \
             info {path}`)."
        )
    })
}

/// The refusal of `store prune`/`store compact` when it would make `marks` unreachable and
/// `--drop-marks` is missing: the marks by name, why they matter, how to go on. Exit 1.
fn marks_refusal(command: &str, dir: &str, marks: &[String]) -> i32 {
    let names = marks
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    errln!(
        "Error: `store {command}` would make the mark(s) {names} unreachable: it removes the \
         history they need, so they cannot be opened, forked or rolled back to afterwards (only \
         from a backup that holds older snapshots). Nothing was changed.\nHelp: run it again with \
         --drop-marks to {command} anyway, or back the store up first (`graphersal store backup \
         {dir} <BACKUP_DIR>`)."
    );
    EXIT_QUERY_ERROR
}

fn fail(err: PersistError) -> i32 {
    errln!("{}", graphersal::error::format_diagnostic(&err));
    EXIT_QUERY_ERROR
}

fn usage(message: &str) -> i32 {
    errln!("{message}\n\nRun `graphersal store help` for usage.");
    EXIT_USAGE_ERROR
}

/// Runs `graphersal store <args>`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let Some(command) = args.first() else {
        out!("{STORE_HELP}");
        return EXIT_USAGE_ERROR;
    };
    let rest = &args[1..];
    // `store <COMMAND> --help | -h` and `store help <COMMAND>`: that command's usage.
    let asked_help = rest.iter().any(|a| a == "--help" || a == "-h");
    let help_for = match command.as_str() {
        "help" | "--help" | "-h" => rest.first().map(String::as_str),
        other if asked_help => Some(other),
        _ => None,
    };
    if let Some(topic) = help_for {
        return match command_help(topic) {
            Some(text) => {
                out!("{text}");
                EXIT_OK
            }
            None => usage(&format!("Unknown store command: {topic}")),
        };
    }
    let mut positional = Vec::new();
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let arg = &rest[i];
        if let Some(flag) = arg.strip_prefix("--") {
            let (name, inline) = match flag.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (flag.to_string(), None),
            };
            let takes_value = matches!(
                name.as_str(),
                "from"
                    | "chunk-size"
                    | "on-damage"
                    | "name"
                    | "up-to"
                    | "at-commit"
                    | "at-time"
                    | "at-mark"
                    | "commit"
                    | "to"
                    | "min-ratio"
                    | "min-reclaimable"
                    | "min-size"
            );
            let value = if takes_value && inline.is_none() {
                i += 1;
                match rest.get(i) {
                    Some(v) => Some(v.clone()),
                    None => return usage(&format!("Option --{name} requires a value")),
                }
            } else {
                inline
            };
            flags.push((name, value));
        } else {
            positional.push(arg.clone());
        }
        i += 1;
    }
    let flag = |name: &str| {
        flags
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone().unwrap_or_default())
    };
    let known: &[&str] = match command.as_str() {
        "create" => &["from", "chunk-size", "single-file", "on-damage"],
        "convert" => &["single-file"],
        "upgrade" => &["single-file", "in-place"],
        "compact-advice" => &["min-ratio", "min-reclaimable", "min-size"],
        "checkpoint" => &["name"],
        "prune" => &["up-to", "drop-marks"],
        "compact" => &["drop-marks"],
        "fork" | "rollback" => &["at-commit", "at-time", "at-mark", "delete"],
        "repair" => &["to"],
        "backup" => &["zip", "full", "from-memory", "without-models"],
        "export" => &["commit", "without-models"],
        _ => &[],
    };
    if let Some((unknown, _)) = flags.iter().find(|(n, _)| !known.contains(&n.as_str())) {
        return usage(&format!(
            "Unknown option for `store {command}`: --{unknown}"
        ));
    }
    let need = |n: usize| -> Result<(), i32> {
        if positional.len() == n {
            Ok(())
        } else {
            Err(usage(&format!(
                "`store {command}` takes {n} argument{}",
                if n == 1 { "" } else { "s" }
            )))
        }
    };
    let number = |name: &str| -> Result<Option<u64>, i32> {
        match flag(name) {
            None => Ok(None),
            Some(raw) => raw.trim().parse::<u64>().map(Some).map_err(|_| {
                usage(&format!(
                    "--{name} expects a non-negative integer, not {raw:?}"
                ))
            }),
        }
    };
    let target = || -> Result<RecoveryTarget, i32> {
        Ok(if let Some(seq) = number("at-commit")? {
            RecoveryTarget::CommitSeq(seq)
        } else if let Some(time) = flag("at-time") {
            RecoveryTarget::Time(
                parse_time(&time).map_err(|why| usage(&format!("--at-time: {why}")))?,
            )
        } else if let Some(mark) = flag("at-mark") {
            RecoveryTarget::Mark(mark)
        } else {
            RecoveryTarget::Latest
        })
    };
    macro_rules! try_code {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(code) => return code,
            }
        };
    }
    match command.as_str() {
        "help" | "--help" | "-h" => {
            out!("{STORE_HELP}");
            EXIT_OK
        }
        "create" => {
            try_code!(need(1));
            let dir = &positional[0];
            if let Some(refusal) = create_refusal(dir) {
                errln!("{refusal}");
                return EXIT_QUERY_ERROR;
            }
            let backend = backend_of(dir, flag("single-file").is_some());
            let mut options = StoreOptions::new();
            if let Some(size) = try_code!(number("chunk-size")) {
                options = options.with_chunk_bytes(u32::try_from(size).unwrap_or(u32::MAX));
            }
            if let Some(raw) = flag("on-damage") {
                match DamagePolicy::parse(&raw) {
                    Ok(policy) => options = options.with_damage_policy(policy),
                    Err(why) => return usage(&format!("--on-damage: {why}")),
                }
            }
            let created = match flag("from") {
                None => Store::create(&backend, options),
                Some(source) if source.to_ascii_lowercase().ends_with(".gsnap") => {
                    match std::fs::File::open(&source) {
                        Ok(file) => Store::create_from_packed(
                            &backend,
                            std::io::BufReader::new(file),
                            options,
                        ),
                        Err(err) => {
                            errln!("Error: cannot open '{source}': {err}");
                            return EXIT_USAGE_ERROR;
                        }
                    }
                }
                Some(source) => {
                    let graph = match GraphSpec::parse(&source).load() {
                        Ok(graph) => graph.into_inner(),
                        Err(err) => {
                            errln!("{err}");
                            return EXIT_USAGE_ERROR;
                        }
                    };
                    Store::create_with(&backend, graph, options)
                }
            };
            match created.and_then(|store| {
                let info = store.info();
                store.close()?;
                info
            }) {
                Ok(info) => {
                    outln!(
                        "Created the {} {dir}: graph {}, commit {}, {} snapshot(s); damage \
                         policy {} (fixed for its life).",
                        if info.space.is_some() {
                            "single-file store"
                        } else {
                            "store"
                        },
                        uuid_text(info.graph_id),
                        info.commit_seq,
                        info.snapshots.len(),
                        info.damage_policy
                    );
                    outln!("Use it: graphersal --graph {dir} (or --server)");
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "info" => {
            try_code!(need(1));
            match Store::inspect(&positional[0]) {
                Ok(info) => {
                    outln!("store       {}", info.location);
                    outln!("format      version {}", info.format_version);
                    outln!("graph id    {}", uuid_text(info.graph_id));
                    for entry in info.lineage.iter().skip(1) {
                        outln!("  parent    {}", uuid_text(entry.graph_id));
                    }
                    if let Some(first) = info.lineage.first()
                        && first.branched_at > 0
                    {
                        outln!("  branched  at commit {}", first.branched_at);
                    }
                    if let Some(marker) = info.backup {
                        outln!(
                            "backup      up to commit {}, taken {} (read-only; `graphersal \
                             store restore {}` makes it the live store)",
                            marker.commit_seq,
                            format_time(marker.taken_at),
                            positional[0]
                        );
                    }
                    outln!(
                        "state       {}",
                        if info.clean {
                            "closed cleanly"
                        } else {
                            "open (in use, or not closed cleanly)"
                        }
                    );
                    outln!(
                        "commit      {} ({})",
                        info.commit_seq,
                        format_time(info.last_commit_at)
                    );
                    outln!("snapshots   {}", info.snapshots.len());
                    if let Some(last) = info.snapshots.last() {
                        outln!(
                            "  latest    commit {} ({} vertices, {} edges)",
                            last.commit_seq,
                            last.vertex_count,
                            last.edge_count
                        );
                    }
                    outln!("marks       {}", info.marks.len());
                    outln!("definitions {}{}", info.definitions, {
                        // Per kind: "(query 2, compression 1)".
                        let kinds: Vec<String> = info
                            .definitions_by_kind
                            .iter()
                            .map(|(kind, count)| format!("{kind} {count}"))
                            .collect();
                        if kinds.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", kinds.join(", "))
                        }
                    });
                    outln!(
                        "wal         {} segment(s), {} bytes",
                        info.wal_segments,
                        info.wal_bytes
                    );
                    if !info.models.is_empty() || !info.schema_models.is_empty() {
                        outln!(
                            "models      {} ({}; every copy carries them)",
                            info.models.len(),
                            bytes_text(info.models.iter().map(|m| m.bytes).sum())
                        );
                        for model in &info.models {
                            let in_use = info.schema_models.iter().any(|u| same_model(u, model));
                            outln!(
                                "  {}{}",
                                model_line(model),
                                if in_use { ", used by the schema" } else { "" }
                            );
                        }
                        for identity in &info.schema_models {
                            if !info.models.iter().any(|m| same_model(identity, m)) {
                                outln!("  missing {identity} (the schema names it)");
                            }
                        }
                    }
                    match info.space {
                        Some(space) => outln!(
                            "file        single file, {} ({} live, {} free space inside: \
                             `store compact-advice`)",
                            bytes_text(space.total_bytes),
                            bytes_text(space.live_bytes),
                            bytes_text(space.garbage_bytes())
                        ),
                        None => outln!("file        a directory"),
                    }
                    // The parameters fixed when the store was created (book: Persistence,
                    // Creation parameters).
                    outln!("fixed at creation (never changed):");
                    outln!("  store id      {}", uuid_text(info.store_id));
                    outln!("  created       {}", format_time(info.created_at));
                    outln!("  chunk size    {} bytes", info.chunk_bytes);
                    outln!(
                        "  on damage     {} ({})",
                        info.damage_policy,
                        match info.damage_policy {
                            DamagePolicy::Continue => {
                                "damage found while open is reported, commits continue"
                            }
                            _ => "damage found while open makes the store read-only",
                        }
                    );
                    outln!(
                        "  backend       {} (format version {})",
                        if info.space.is_some() {
                            "single file"
                        } else {
                            "directory"
                        },
                        info.format_version
                    );
                    if !info.attic.is_empty() {
                        outln!("attic       {} entr(y/ies)", info.attic.len());
                    }
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "list" => {
            try_code!(need(1));
            match Store::snapshots_dir(&positional[0]) {
                Ok(snapshots) => {
                    outln!(
                        "{:>12}  {:<20}  {:>10}  {:>10}  name",
                        "commit",
                        "last commit",
                        "vertices",
                        "edges"
                    );
                    for s in snapshots {
                        outln!(
                            "{:>12}  {:<20}  {:>10}  {:>10}  {}",
                            s.commit_seq,
                            format_time(s.last_commit_at),
                            s.vertex_count,
                            s.edge_count,
                            s.name
                        );
                    }
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "models" => {
            if positional.first().map(String::as_str) == Some("remove") {
                if positional.len() != 3 {
                    return usage("`store models remove` takes the store and the model's identity");
                }
                return match Store::remove_model_dir(positional[1].as_str(), &positional[2]) {
                    Ok(model) => {
                        outln!("Removed {} ({}).", model_line(&model), model.id);
                        EXIT_OK
                    }
                    Err(err) => fail(err),
                };
            }
            try_code!(need(1));
            let used = Store::inspect(positional[0].as_str())
                .map(|info| info.schema_models)
                .unwrap_or_default();
            match Store::models_dir(positional[0].as_str()) {
                Ok(models) if models.is_empty() => {
                    errln!(
                        "{} keeps no embedding models (add one with `graphersal embed-model \
                         download <REPO> --into {}`)",
                        positional[0],
                        positional[0]
                    );
                    for identity in &used {
                        outln!("missing  {identity}  (the schema names it)");
                    }
                    EXIT_OK
                }
                Ok(models) => {
                    for model in &models {
                        let in_use = used.iter().any(|u| same_model(u, model));
                        outln!(
                            "{}  {}",
                            model_line(model),
                            if in_use {
                                "used by the schema"
                            } else {
                                "not used by the schema"
                            }
                        );
                    }
                    for identity in &used {
                        if !models.iter().any(|m| same_model(identity, m)) {
                            outln!(
                                "missing  {identity}  (the schema names it, the store holds no such model)"
                            );
                        }
                    }
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "marks" => {
            try_code!(need(1));
            match Store::marks_dir(&positional[0]) {
                Ok(marks) => {
                    for mark in marks {
                        outln!(
                            "{:>12}  {:<20}  {}",
                            mark.commit_seq(),
                            format_time(mark.time()),
                            mark.name()
                        );
                    }
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "mark" => {
            try_code!(need(2));
            match open_rw(&positional[0]).and_then(|store| {
                let mark = store.mark(&positional[1])?;
                store.close()?;
                Ok(mark)
            }) {
                Ok(mark) => {
                    outln!("Mark \"{}\" at commit {}.", mark.name(), mark.commit_seq());
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "checkpoint" => {
            try_code!(need(1));
            let name = flag("name");
            match Store::checkpoint_dir(&positional[0], StoreOptions::new(), name.as_deref()) {
                Ok(report) => {
                    let info = &report.snapshot;
                    outln!(
                        "Snapshot at commit {} ({} vertices, {} edges).",
                        info.commit_seq,
                        info.vertex_count,
                        info.edge_count
                    );
                    if !report.written {
                        outln!("Nothing was committed after it: no new snapshot written.");
                    } else if report.merged {
                        outln!(
                            "Merged from the snapshot at commit {} and {} commits: {} segment \
                             files reused, {} copied, {} written.",
                            report.base_commit_seq,
                            report.commits_merged,
                            report.segments_reused,
                            report.segments_copied,
                            report.segments_written
                        );
                    } else {
                        outln!(
                            "Encoded from the loaded graph (the WAL since the last snapshot is \
                             larger than the merge bound)."
                        );
                    }
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "verify" => {
            try_code!(need(1));
            match Store::verify_dir(&positional[0]) {
                Ok(report) => {
                    outln!(
                        "{} snapshot(s), {} WAL segment(s), {} record(s), commits {}..{}",
                        report.snapshots_checked,
                        report.segments_checked,
                        report.records_checked,
                        report.first_commit,
                        report.last_commit
                    );
                    for note in &report.notes {
                        outln!("note: {note}");
                    }
                    for problem in &report.problems {
                        outln!(
                            "DAMAGE: {}{}: {}",
                            problem.path,
                            problem
                                .offset
                                .map(|o| format!(" at byte {o}"))
                                .unwrap_or_default(),
                            problem.reason
                        );
                    }
                    if report.is_ok() {
                        outln!("ok: no damage found");
                        EXIT_OK
                    } else {
                        if let Ok(damage) = Store::damage_report_dir(&positional[0])
                            && !damage.is_empty()
                        {
                            out!("{damage}");
                        }
                        errln!(
                            "Error: {} problem(s) found. Help: keep the store's files (do not prune); \
                             build a repaired copy with `graphersal store repair <dir> --to \
                             <new_dir>`, restore a backup, or fork a target before the damage.",
                            report.problems.len()
                        );
                        EXIT_QUERY_ERROR
                    }
                }
                // Both GRAPH copies damaged: verify cannot read the identity, the damage scan
                // can (it rebuilds the lineage from the snapshots and WAL headers).
                Err(err) if matches!(Store::is_store(positional[0].as_str()), Ok(true)) => {
                    match Store::damage_report_dir(positional[0].as_str()) {
                        Ok(damage) if !damage.is_empty() => {
                            out!("{damage}");
                            errln!(
                                "Error: the store's GRAPH file cannot be read ({err}). Help: keep \
                                 the store's files (do not prune); \
                                 build a repaired copy with `graphersal store repair <dir> --to \
                                 <new_dir>`, or restore a backup."
                            );
                            EXIT_QUERY_ERROR
                        }
                        _ => fail(err),
                    }
                }
                Err(err) => fail(err),
            }
        }
        "prune" => {
            try_code!(need(1));
            let Some(up_to) = try_code!(number("up-to")) else {
                return usage(
                    "`store prune` needs --up-to <COMMIT> (nothing is pruned implicitly)",
                );
            };
            // A mark is never lost without a question: the estimate names them first.
            if flag("drop-marks").is_none() {
                match Store::prune_estimate_dir(positional[0].as_str(), up_to) {
                    Ok(estimate) if !estimate.marks_unreachable.is_empty() => {
                        return marks_refusal("prune", &positional[0], &estimate.marks_unreachable);
                    }
                    Ok(_) => {}
                    Err(err) => return fail(err),
                }
            }
            let pruned = match Store::backup_marker_dir(&positional[0]) {
                Ok(Some(_)) => Store::prune_backup(&positional[0], up_to),
                _ => open_rw(&positional[0]).and_then(|store| {
                    let report = store.prune(up_to)?;
                    store.close()?;
                    Ok(report)
                }),
            };
            match pruned {
                Ok(report) => {
                    outln!(
                        "Kept from commit {}; removed {} snapshot(s){} and {} WAL segment(s).",
                        report.kept_from,
                        report.snapshots_removed.len(),
                        commit_list(&report.snapshots_removed),
                        report.segments_removed.len()
                    );
                    if !report.marks_unreachable.is_empty() {
                        outln!(
                            "No longer reachable (their history was pruned): the mark(s) {}.",
                            report.marks_unreachable.join(", ")
                        );
                    }
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "fork" => {
            try_code!(need(2));
            let target = try_code!(target());
            match Store::fork_dir(&positional[0], target, &positional[1]) {
                Ok(report) => {
                    let info = &report.snapshot;
                    outln!(
                        "Forked into {} at commit {}: graph {} (parent {}).",
                        positional[1],
                        info.commit_seq,
                        uuid_text(info.graph_id),
                        uuid_text(info.parent_graph_id)
                    );
                    print_marks_not_carried(&report.marks_not_carried, &positional[0]);
                    outln!(
                        "Open it: graphersal --graph {} --server (or the REPL: graphersal \
                         --graph {}).",
                        positional[1],
                        positional[1]
                    );
                    outln!(
                        "{} is unchanged; to undo the fork, delete the directory {}.",
                        positional[0],
                        positional[1]
                    );
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "backup" => {
            try_code!(need(2));
            let (dir, target) = (&positional[0], &positional[1]);
            if flag("from-memory").is_some() {
                return usage(&format!(
                    "--from-memory backs up the graph of a store OPEN IN THIS PROCESS (whose files \
                     are damaged), and `graphersal store backup` runs in a new process: use the \
                     Store menu of the dev server that has {dir} open (Back up from memory), its \
                     endpoint POST /api/store/backup {{\"dir\": \"{target}\", \"from_memory\": \
                     true}}, or Python store.backup(path, from_memory=True)"
                ));
            }
            if flag("zip").is_some() {
                if flag("full").is_some() {
                    return usage("--zip backups are always full: drop --full");
                }
                if Path::new(target).exists() {
                    return usage(&format!("'{target}' exists already: choose a new file"));
                }
                // Read and written: the archive is read back and checked after it was written.
                let file = match std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(target)
                {
                    Ok(file) => file,
                    Err(err) => {
                        errln!("Error: cannot create '{target}': {err}");
                        return EXIT_USAGE_ERROR;
                    }
                };
                let models = if flag("without-models").is_some() {
                    ModelCopy::Exclude
                } else {
                    ModelCopy::Include
                };
                let result = Store::backup_zip_dir_with(dir, file, models);
                if result.is_ok() {
                    // Opened for writing: Windows refuses to flush a read-only handle.
                    let _ = std::fs::OpenOptions::new()
                        .write(true)
                        .open(target)
                        .and_then(|f| f.sync_all());
                }
                if result.is_err() {
                    let _ = std::fs::remove_file(target);
                }
                return match result {
                    Ok(report) => {
                        outln!(
                            "Full backup of snapshot {} and the WAL up to commit {}: {} file(s), \
                             {} bytes, in {target}; every checksum read back.",
                            report.snapshot_commit_seq,
                            report.commit_seq,
                            report.files.len(),
                            report.bytes
                        );
                        out!("{}", models_carried(&report.models));
                        for warning in &report.warnings {
                            errln!("warning: {warning}");
                        }
                        EXIT_OK
                    }
                    Err(err) => fail(err),
                };
            }
            if flag("without-models").is_some() {
                return usage(
                    "--without-models applies to a ZIP backup (--zip): a backup directory is \
                     a copy of the whole store, models included",
                );
            }
            let mode = if flag("full").is_some() {
                BackupMode::Full
            } else {
                BackupMode::Auto
            };
            match Store::backup_dir_with(dir, target, mode) {
                Ok(report) => {
                    out!("{}", backup_summary(&report, target));
                    for warning in &report.warnings {
                        errln!("warning: {warning}");
                    }
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "restore" => {
            if positional.len() == 1 {
                let dir = &positional[0];
                if Path::new(dir).is_file()
                    && matches!(SingleFileDir::is_single_file(dir), Ok(false))
                {
                    return usage(&format!(
                        "'{dir}' is a file: unpack a ZIP backup with `graphersal store restore \
                         {dir} <NEW_DIR>`"
                    ));
                }
                return match Store::restore_backup(dir) {
                    Ok(marker) => {
                        outln!(
                            "Restored: {dir} is now the live store (the backup up to commit {}, \
                             taken {}; the same graph id). Use it: graphersal --graph {dir}",
                            marker.commit_seq,
                            format_time(marker.taken_at)
                        );
                        EXIT_OK
                    }
                    Err(err) => fail(err),
                };
            }
            try_code!(need(2));
            let file = match std::fs::File::open(&positional[0]) {
                Ok(file) => file,
                Err(err) => {
                    errln!("Error: cannot open '{}': {err}", positional[0]);
                    return EXIT_USAGE_ERROR;
                }
            };
            match Store::restore_zip(std::io::BufReader::new(file), &positional[1]) {
                Ok(()) => {
                    outln!(
                        "Restored into {} (the live store). Use it: graphersal --graph {}",
                        positional[1],
                        positional[1]
                    );
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "export" => {
            try_code!(need(2));
            let commit = try_code!(number("commit"));
            let target = &positional[1];
            let file = match std::fs::File::create(target) {
                Ok(file) => file,
                Err(err) => {
                    errln!("Error: cannot create '{target}': {err}");
                    return EXIT_USAGE_ERROR;
                }
            };
            let (models, carried) = if flag("without-models").is_some() {
                (ModelCopy::Exclude, Vec::new())
            } else {
                (
                    ModelCopy::Include,
                    Store::models_dir(positional[0].as_str()).unwrap_or_default(),
                )
            };
            match Store::export_packed_dir_with(
                &positional[0],
                commit,
                std::io::BufWriter::new(file),
                models,
            ) {
                Ok(info) => {
                    outln!(
                        "Exported the snapshot at commit {} to {target}.",
                        info.commit_seq
                    );
                    out!("{}", models_carried(&carried));
                    EXIT_OK
                }
                Err(err) => {
                    let _ = std::fs::remove_file(target);
                    fail(err)
                }
            }
        }
        "rollback" => {
            try_code!(need(1));
            let target = try_code!(target());
            if target == RecoveryTarget::Latest {
                return usage(
                    "`store rollback` needs a target: --at-commit, --at-time or --at-mark",
                );
            }
            let mode = if flag("delete").is_some() {
                RollbackMode::Delete
            } else {
                RollbackMode::Move
            };
            match open_rw(&positional[0]).and_then(|store| {
                let report = store.rollback_to(target, mode)?;
                store.close()?;
                Ok(report)
            }) {
                Ok(report) => {
                    outln!(
                        "Rolled back from commit {} to commit {}: new lineage {}.",
                        report.previous_commit_seq,
                        report.target,
                        uuid_text(report.graph_id)
                    );
                    let dir = &positional[0];
                    let range = format!(
                        "commits {}..={}, {} snapshot(s), {} WAL file(s)",
                        report.target + 1,
                        report.previous_commit_seq,
                        report.snapshots_moved.len(),
                        report.segments_moved
                    );
                    match &report.attic {
                        Some(id) => {
                            outln!("The history after it ({range}) is in the attic as {id}.");
                            outln!(
                                "Undo (only while nothing new is committed or marked): \
                                 graphersal store attic {dir} restore {id}"
                            );
                            outln!(
                                "Keep it as a new store instead: graphersal store attic {dir} \
                                 fork {id} <NEW_DIR>; delete it: graphersal store attic {dir} \
                                 remove {id}"
                            );
                        }
                        None => outln!(
                            "The history after it ({range}) was deleted: this cannot be undone."
                        ),
                    }
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "attic" => run_attic(&positional),
        "convert" => {
            try_code!(need(2));
            let (from, to) = (&positional[0], &positional[1]);
            if Path::new(to).exists()
                && !Path::new(to)
                    .read_dir()
                    .is_ok_and(|mut entries| entries.next().is_none())
            {
                return usage(&format!(
                    "'{to}' exists already: choose a new directory or file"
                ));
            }
            let target = backend_of(to, flag("single-file").is_some());
            match Store::convert(from.as_str(), &target) {
                Ok((files, bytes)) => {
                    outln!(
                        "Copied the store {from} into the {} {to}: {files} file(s), {}; verified.",
                        if target.space_usage().is_some() {
                            "single file"
                        } else {
                            "directory"
                        },
                        bytes_text(bytes)
                    );
                    outln!("Use it: graphersal --graph {to}. {from} is unchanged.");
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "upgrade" => run_upgrade(
            &positional,
            flag("in-place").is_some(),
            flag("single-file").is_some(),
        ),
        "compact" => {
            try_code!(need(1));
            let dir = &positional[0];
            let drop_marks = flag("drop-marks").is_some();
            let compacted = open_rw(dir.as_str()).and_then(|store| {
                // A mark is never lost without a question: the advice names them first.
                if !drop_marks {
                    let lost = store.compaction_advice()?.prune.marks_unreachable;
                    if !lost.is_empty() {
                        store.close()?;
                        return Ok(Err(lost));
                    }
                }
                let report = store.compact()?;
                store.close()?;
                Ok(Ok(report))
            });
            match compacted {
                Ok(Err(lost)) => marks_refusal("compact", dir, &lost),
                Ok(Ok(report)) => {
                    outln!(
                        "Pruned: kept from commit {}; removed {} snapshot(s) and {} WAL segment(s).",
                        report.prune.kept_from,
                        report.prune.snapshots_removed.len(),
                        report.prune.segments_removed.len()
                    );
                    if !report.prune.marks_unreachable.is_empty() {
                        outln!(
                            "No longer reachable (their history was pruned): the mark(s) {}.",
                            report.prune.marks_unreachable.join(", ")
                        );
                    }
                    match report.compaction {
                        Some(c) => outln!(
                            "Compacted the file: {} -> {} ({} given back).",
                            bytes_text(c.bytes_before),
                            bytes_text(c.bytes_after),
                            bytes_text(report.bytes_freed())
                        ),
                        None => outln!(
                            "A directory store: the prune gave the space back (nothing to compact)."
                        ),
                    }
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "compact-advice" => {
            try_code!(need(1));
            let mut policy = CompactionPolicy::new();
            if let Some(raw) = flag("min-ratio") {
                match raw.trim().parse::<f64>() {
                    Ok(ratio) if (0.0..=1.0).contains(&ratio) => {
                        policy = policy.with_min_garbage_ratio(ratio);
                    }
                    _ => return usage(&format!("--min-ratio expects 0.0..1.0, not {raw:?}")),
                }
            }
            if let Some(bytes) = try_code!(number("min-reclaimable")) {
                policy = policy.with_min_reclaimable_bytes(bytes);
            }
            if let Some(bytes) = try_code!(number("min-size")) {
                policy = policy.with_min_total_bytes(bytes);
            }
            match Store::compaction_advice_dir(positional[0].as_str(), policy) {
                Ok(advice) => {
                    out!("{}", advice_text(&advice, &positional[0]));
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "repair" => {
            try_code!(need(1));
            let Some(to) = flag("to").filter(|t| !t.is_empty()) else {
                return usage(
                    "`store repair` needs --to <NEW_DIR> (a store is never repaired in place)",
                );
            };
            match Store::repair_dir(&positional[0], &to) {
                Ok(report) => {
                    out!("{}", report.damage);
                    out!("{report}");
                    if report.is_lossless() {
                        outln!("ok: nothing lost. Use it: graphersal --graph {to}");
                        EXIT_OK
                    } else {
                        errln!(
                            "Error: the repaired store in {to} misses data (see lost and diverged \
                             above). Help: check the diverged elements by hand, or restore a \
                             backup; the damaged store {} is unchanged.",
                            positional[0]
                        );
                        EXIT_QUERY_ERROR
                    }
                }
                Err(err) => fail(err),
            }
        }
        other => usage(&format!("Unknown store command: {other}")),
    }
}

/// `graphersal store upgrade`: into a copy (`<DIR> <NEW>`) or `--in-place`.
fn run_upgrade(positional: &[String], in_place: bool, single_file: bool) -> i32 {
    let report = match (in_place, positional) {
        (true, [dir]) => {
            if single_file {
                return usage(
                    "`store upgrade --in-place` keeps the store's backend: drop --single-file",
                );
            }
            Store::upgrade_in_place(dir.as_str())
        }
        (false, [from, to]) => {
            if Path::new(to).exists()
                && !Path::new(to)
                    .read_dir()
                    .is_ok_and(|mut entries| entries.next().is_none())
                && !Path::new(to).join("upgrade-in-progress.tmp").exists()
            {
                return usage(&format!(
                    "'{to}' exists already: choose a new directory or file"
                ));
            }
            Store::upgrade(from.as_str(), backend_of(to, single_file))
        }
        (true, _) => return usage("`store upgrade <DIR> --in-place` takes 1 argument"),
        (false, _) => {
            return usage("`store upgrade` takes <DIR> <NEW> (a new store), or <DIR> --in-place");
        }
    };
    let report = match report {
        Ok(report) => report,
        Err(err) => return fail(err),
    };
    let dir = &positional[0];
    if report.already_current {
        outln!(
            "{dir} already has the current store format (version {}): nothing to do.",
            report.to_version
        );
        return EXIT_OK;
    }
    let what = if report.backup.is_some() {
        "backup"
    } else {
        "store"
    };
    let verify = &report.verify;
    outln!(
        "Verified the {what} {dir} (store format version {}): {} snapshot(s), {} WAL segment(s), \
         {} record(s).",
        report.from_version,
        verify.snapshots_checked,
        verify.segments_checked,
        verify.records_checked
    );
    if report.in_place {
        outln!(
            "Upgraded {dir} in place to store format version {}: GRAPH rewritten, every other \
             file kept. The older Graphersal no longer opens it.",
            report.to_version
        );
    } else {
        let to = &positional[1];
        outln!(
            "Upgraded into {to} (store format version {}): {} file(s), {}; verified. {dir} is \
             unchanged (keep using {to}: it is the same store, with the same identity).",
            report.to_version,
            report.files_copied,
            bytes_text(report.bytes_copied)
        );
    }
    let target = if report.in_place { dir } else { &positional[1] };
    if report.backup.is_some() {
        outln!("It stays a backup: graphersal store restore {target} makes it the live store.");
    } else {
        outln!("Use it: graphersal --graph {target}");
    }
    EXIT_OK
}

/// The marks a fork or repair did not carry, one line each (`name @ commit N`), with where they
/// stay usable.
fn print_marks_not_carried(marks: &[graphersal::changes::Mark], source: &str) {
    if marks.is_empty() {
        return;
    }
    outln!(
        "Marks not carried ({}; they stay targets of {source} only):",
        marks.len()
    );
    for mark in marks {
        outln!("  {} @ commit {}", mark.name(), mark.commit_seq());
    }
}

/// `graphersal store attic <DIR> [list | changes <ID> | restore <ID> | fork <ID> <NEW_DIR> |
/// remove <ID>]`.
fn run_attic(positional: &[String]) -> i32 {
    let Some(dir) = positional.first() else {
        return usage("`store attic` takes the store directory");
    };
    let action = positional.get(1).map(String::as_str).unwrap_or("list");
    let id = positional.get(2);
    let need_id = || {
        usage(&format!(
            "`store attic <DIR> {action}` takes an entry id (graphersal store attic {dir} lists them)"
        ))
    };
    match action {
        "list" => match Store::attic_list_dir(dir) {
            Ok(entries) => {
                if entries.is_empty() {
                    outln!("The attic is empty.");
                }
                for e in entries {
                    outln!(
                        "{}  commits {}..={} rolled back at {} ({}), {}{}",
                        e.id,
                        e.target + 1,
                        e.last_commit_seq,
                        format_time(e.time),
                        e.reason,
                        bytes_text(e.bytes),
                        if e.snapshots.is_empty() {
                            String::new()
                        } else {
                            format!(", {} snapshot(s)", e.snapshots.len())
                        }
                    );
                }
                EXIT_OK
            }
            Err(err) => fail(err),
        },
        "changes" => {
            let Some(id) = id else { return need_id() };
            match Store::attic_changes_dir(dir, id) {
                Ok(changes) => {
                    for change in changes {
                        match change {
                            Ok(c) => outln!(
                                "commit {:>8}  {}  {} mutation(s){}",
                                c.commit_seq(),
                                format_time(c.committed_at()),
                                c.mutations().len(),
                                c.metadata()
                                    .principal()
                                    .map(|p| format!(" by {p}"))
                                    .unwrap_or_default()
                            ),
                            Err(err) => return fail(err),
                        }
                    }
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "restore" | "remove" => {
            let Some(id) = id else { return need_id() };
            match open_rw(dir).and_then(|store| {
                let done = if action == "restore" {
                    Some(store.attic_restore(id)?)
                } else {
                    store.attic_remove(id)?;
                    None
                };
                store.close()?;
                Ok(done)
            }) {
                Ok(Some(commit)) => {
                    outln!("Restored {id}: the store is at commit {commit} again.");
                    EXIT_OK
                }
                Ok(None) => {
                    outln!("Removed {id} from the attic.");
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "fork" => {
            let (Some(id), Some(new_dir)) = (id, positional.get(3)) else {
                return usage("`store attic <DIR> fork <ID> <NEW_DIR>`");
            };
            match Store::attic_fork_dir(dir, id, new_dir) {
                Ok(report) => {
                    let info = &report.snapshot;
                    outln!(
                        "Forked {id} into {new_dir} at commit {}: graph {} (parent {}).",
                        info.commit_seq,
                        uuid_text(info.graph_id),
                        uuid_text(info.parent_graph_id)
                    );
                    print_marks_not_carried(&report.marks_not_carried, dir);
                    outln!(
                        "Open it: graphersal --graph {new_dir} --server. The attic entry stays; \
                         to undo the fork, delete the directory {new_dir}."
                    );
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        other => usage(&format!(
            "Unknown attic action: {other} (list, changes, restore, fork, remove)"
        )),
    }
}

/// The commits of a list as plain text for a message: ` (at commits 2, 5)`, or nothing when empty.
fn commit_list(commits: &[u64]) -> String {
    if commits.is_empty() {
        return String::new();
    }
    let text: Vec<String> = commits.iter().map(u64::to_string).collect();
    format!(" (at commits {})", text.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_ids_are_formatted() {
        assert_eq!(format_time(0), "-");
        assert_eq!(format_time(1_760_000_000_000_000), "2025-10-09T08:53:20Z");
        assert_eq!(uuid_text(1), "00000000-0000-0000-0000-000000000001");
        assert_eq!(bytes_text(512), "512 B");
        assert_eq!(bytes_text(1536), "1.5 KiB");
    }

    #[test]
    fn at_time_takes_rfc_3339_and_microseconds() {
        const T: i64 = 1_760_000_000_000_000; // 2025-10-09T08:53:20Z
        assert_eq!(parse_time("1760000000000000").unwrap(), T);
        // A whole second (minute) covers all of it: the last microsecond.
        assert_eq!(parse_time("2025-10-09T08:53:20Z").unwrap(), T + 999_999);
        assert_eq!(parse_time(" 2025-10-09t08:53:20z ").unwrap(), T + 999_999);
        assert_eq!(parse_time("2025-10-09 08:53:20Z").unwrap(), T + 999_999);
        assert_eq!(
            parse_time("2025-10-09T08:53Z").unwrap(),
            T - 20_000_000 + 59_999_999
        );
        assert_eq!(parse_time("2025-10-09T08:53:20.5Z").unwrap(), T + 599_999);
        assert_eq!(parse_time("2025-10-09T08:53:20.000042Z").unwrap(), T + 42);
        assert_eq!(parse_time("2025-10-09T08:53:20.0000429Z").unwrap(), T + 42);
        assert_eq!(
            parse_time("2025-10-09T10:53:20+02:00").unwrap(),
            T + 999_999
        );
        assert_eq!(parse_time("2025-10-09T10:53:20+0200").unwrap(), T + 999_999);
        assert_eq!(parse_time("2025-10-09T06:53:20-02").unwrap(), T + 999_999);
        assert_eq!(parse_time("1970-01-01T00:00:00Z").unwrap(), 999_999);
        assert_eq!(
            parse_time("2024-02-29T00:00Z").unwrap(),
            1_709_164_800_000_000 + 59_999_999
        );
        // The printed form is accepted back and covers the printed commit.
        let printed = format_time(T + 123_456);
        assert!(parse_time(&printed).unwrap() >= T + 123_456);
        for bad in [
            "",
            "yesterday",
            "2025-10-09",
            "2025-10-09T08:53:20",
            "2025-13-09T08:53Z",
            "2025-02-30T08:53Z",
            "2023-02-29T08:53Z",
            "2025-10-09T24:00Z",
            "2025-10-09T08:60Z",
            "2025-10-09T08:53:20.Z",
            "2025-10-09T08:53:20+2",
            "2025-10-09T08:53:20Zjunk",
            "-5",
        ] {
            let err = parse_time(bad).unwrap_err();
            assert!(err.to_string().contains("RFC 3339"), "{bad}: {err}");
        }
    }

    #[test]
    fn every_command_has_help() {
        for command in [
            "create",
            "info",
            "list",
            "marks",
            "mark",
            "checkpoint",
            "verify",
            "prune",
            "fork",
            "backup",
            "restore",
            "export",
            "rollback",
            "attic",
            "repair",
            "convert",
            "upgrade",
            "compact",
            "compact-advice",
            "help",
        ] {
            let help = command_help(command).unwrap();
            assert!(help.starts_with("USAGE:"), "{command}");
            assert!(
                help.contains(&format!("graphersal store {command}")),
                "{help}"
            );
        }
        assert!(command_help("fork").unwrap().contains("--at-time"));
        assert!(command_help("frobnicate").is_none());
    }
}
