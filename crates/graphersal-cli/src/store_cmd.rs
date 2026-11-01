//! `graphersal store <command> ...`: manage a Store directory (the persistence of one graph;
//! reference: the book page `book/src/persistence/store_cli.md`). Commands that only read
//! (`info`, `list`, `marks`, `verify`, `backup`, `export`, `fork`) work while a writer (the dev
//! server, a REPL) has the store open; the others open it read-write and fail with "in use"
//! then. A backup directory (marked in its `GRAPH` file) is read-only: the reading commands and
//! `prune` work on it, `restore <DIR>` makes it the live store.

use std::path::Path;

use std::sync::Arc;

use graphersal::persist::{
    BackupMode, BackupReport, BlobSource, ClosedCommitOptions, CompactionAdvice, CompactionPolicy,
    DamagePolicy, ModelCopy, ModelInfo, PersistError, RecoveryTarget, RepairOptions,
    RestoreBlobsReport, RollbackMode, SingleFileDir, Store, StoreDir, StoreOptions, store_dir_at,
};

use crate::cli::{EXIT_OK, EXIT_QUERY_ERROR, EXIT_USAGE_ERROR, GraphSpec};
use crate::out::{Note, print_block, use_it};
use graphersal::system_journal::{JournalQuery, Level};
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
    graphersal store journal <DIR> [--level <L>] [--since <TIME>] [--source <P>] [--kind <K>]
                                     [--after <SEQ> | --last <N>] [--limit <N>] [--follow] [--json]
                                     the system journal: what the store did (opens, backups,
                                     checkpoints, damage, ...); works while it is in use
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
    graphersal store models remove <DIR> <REPO>
                                     remove a model from the store's current state: one commit
                                     appended without loading the graph
    graphersal store models default <DIR> <REPO>|--none
                                     make the model of REPO the default (the one embed() uses
                                     without a name), or none (hash-ngram/1): one commit
    graphersal store restore-blobs <DIR> --from <SOURCE> [--json]
                                     copy missing or damaged blobs (models) back from a backup
                                     (directory or .zip), a .gsnap or another store
    graphersal store rollback <DIR> TARGET [--delete]
                                     roll the store back IN PLACE: everything after TARGET moves
                                     to the attic (--delete: deleted); a new lineage continues
    graphersal store attic <DIR>                    the attic entries (rolled-back history)
    graphersal store attic <DIR> changes <ID>       the commits of an attic entry
    graphersal store attic <DIR> restore <ID>       undo its rollback (only while nothing was
                                     committed since)
    graphersal store attic <DIR> fork <ID> <NEW_DIR>  a new store from the end of the entry
    graphersal store attic <DIR> remove <ID>        delete an attic entry for good
    graphersal store repair <DIR> --to <NEW_DIR> [--drop-damaged-blobs]
                                     build a repaired copy of a damaged store in a NEW directory
                                     (the damaged files are never changed); --drop-damaged-blobs
                                     leaves out definitions whose blobs are missing or damaged

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
Embedding models (blobs in the store, defined in its catalog) enter it only by `graphersal
embed-model download <REPO> --into <DIR>` or `embed-model import <REPO|MODEL_DIR> --into <DIR>`
(a closed store: one commit appended without loading the graph); every copy (backup,
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
            "Converts a store written by an older Graphersal (0.1.x to 0.4.x) to the current\n\
             format; no older binary is needed. With NEW: a new store in NEW (a new directory, or a single\n\
             file: NEW ending in .gstore, or --single-file); DIR is not changed. With\n\
             --in-place: DIR itself (back it up first: afterwards the older Graphersal no longer\n\
             opens it). Everything is carried: the graph, schema, catalog, the history, marks,\n\
             attic entries and the store's identity and creation parameters; only GRAPH and\n\
             GRAPH.copy are rewritten, every other file is kept byte for byte. DIR may be a\n\
             directory, a .gstore file or a backup directory (it stays a backup). DIR is verified\n\
             first: a damaged store is refused (store repair DIR --to NEW writes a repaired\n\
             store of the current format), and so is a store in use (no server on it). The copy\n\
             is verified before it becomes a store; an interrupted upgrade leaves no store in\n\
             NEW (run it again). A ZIP backup of an older store is upgraded by `store restore`.\n\
             The embedding models of a 0.4.0 store (models/) become blobs defined by one\n\
             commit (no default model is set; take a full backup afterwards). It warns about\n\
             models the schema needs that the store lacks, and notes every vector property\n\
             with a model but no source property (\"from\") with the schema patch adding one.",
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
        "journal" => (
            "graphersal store journal <DIR> [--level <L>] [--since <TIME>] [--source <P>] [--kind <K>] [--after <SEQ> | --last <N>] [--limit <N>] [--follow] [--json]",
            "Prints the store's system journal: one line per record (time UTC, seq, level, kind,\n\
             message, fields), oldest first. Reads the files only (no lock, nothing written), so\n\
             it works while a server has the store open. --level error|warn|notice|info shows\n\
             that level and the more severe ones; --since TIME the records at or after TIME;\n\
             --source a source and the ones below it (store matches store.backup); --kind one\n\
             kind; --after SEQ the records after that seq; --last N only the newest N matching\n\
             records (read from the end of the journal, still printed oldest first); --limit at\n\
             most N per page (default 1000).\n\
             --json prints one JSON object per record. --follow keeps printing new records\n\
             (polls once a second) until interrupted.",
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
            "graphersal store models <DIR>\n    graphersal store models remove <DIR> <REPO>\n    \
             graphersal store models default <DIR> <REPO>|--none",
            "Lists the embedding models of the store's current state (catalog definitions, each\n\
             with its files as one blob; format spec 4.5), one section per model with one\n\
             `key: value` per line: the model (its repository), its full name (the identity a\n\
             schema's `embedder` records, model2vec/<repo>@<12 hex>), the size, the licence, where\n\
             the licence text comes from (from the repository: its own licence file;\n\
             standard text: the repository has no licence file, so the store keeps the unchanged\n\
             standard text of the licence the model card names; none: no standard text for this\n\
             licence), whether it is the default, what in the schema uses it, and whether the\n\
             repository is gated. `remove` drops the model of REPO from the\n\
             current state: ONE commit appended to the WAL without loading the graph (format\n\
             spec 12.12; earlier positions keep it, a prune removes its blob later); refused\n\
             while the schema needs it, and with \"in use\" while a writer has the store open.\n\
             `default` makes the model of REPO the default model (the one `embed` and\n\
             `embed(text)` use without a name), `--none` makes none the default (then\n\
             hash-ngram/1 is): one commit the same way.\n\
             A model enters a store by `graphersal embed-model download <REPO> --into <DIR>` or\n\
             `embed-model import <REPO|MODEL_DIR> --into <DIR>`.",
            false,
        ),
        "restore-blobs" => (
            "graphersal store restore-blobs <DIR> --from <SOURCE> [--json]",
            "Copies the blobs (embedding models) the store references that are missing or\n\
             damaged back from SOURCE: a backup directory (full or incremental), a ZIP backup\n\
             (.zip), a packed snapshot (.gsnap) or another store (a directory or .gstore). A blob\n\
             is named by its hash, so any copy of it is the right bytes; each is checked (length,\n\
             CRC-32C, hash) before it is moved into place, and a healthy blob is never touched.\n\
             The store must be closed (\"in use\" otherwise). Prints what was restored, what\n\
             SOURCE lacks and what SOURCE holds damaged too; exit 1 unless every blob the store\n\
             references is intact afterwards. --json prints the report as one JSON object.",
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
            "graphersal store repair <DIR> --to <NEW_DIR> [--drop-damaged-blobs]",
            "Builds a repaired copy of a damaged store in NEW_DIR (never in place; the damaged\n\
             files are not changed) and reports what was repaired, lost and diverged. Exit 1 when\n\
             data is lost. A blob (an embedding model) missing or damaged in DIR is not carried\n\
             and reported; restore it first with `store restore-blobs`, or give\n\
             --drop-damaged-blobs: the copy then leaves out the definitions referencing such a\n\
             blob (refused, nothing written, while the schema needs one of them).",
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

/// What a backup copied, as `store backup` prints it: blocks in the note layout (a headline,
/// one `key: value` per line, commands under a comment), set apart by a blank line.
pub fn backup_summary(report: &BackupReport, target: &str) -> String {
    let list = |items: &[u64]| {
        if items.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = items.iter().map(u64::to_string).collect();
            format!(" [{}]", items.join(", "))
        }
    };
    let note = if !report.incremental {
        Note::new(format!("Full backup into {target}"))
            .detail("snapshot", format!("commit {}", report.snapshot_commit_seq))
            .detail("WAL", format!("up to commit {}", report.commit_seq))
            .detail(
                "WAL segments copied",
                report.segments_copied.len().to_string(),
            )
            .detail("files", report.files.len().to_string())
            .detail("bytes", report.bytes.to_string())
    } else if report.bytes == 0 && !report.lineage_changed {
        Note::new(format!("Incremental backup into {target}")).detail(
            "state",
            format!("current (nothing new since commit {})", report.commit_seq),
        )
    } else {
        let mut note = Note::new(format!("Incremental backup into {target}"))
            .detail(
                "commits",
                format!(
                    "{}..{} ({} new)",
                    report.previous_commit_seq + 1,
                    report.commit_seq,
                    report.commit_seq.saturating_sub(report.previous_commit_seq)
                ),
            )
            .detail(
                "snapshots copied",
                format!(
                    "{}{}",
                    report.snapshots_copied.len(),
                    list(&report.snapshots_copied)
                ),
            )
            .detail(
                "WAL segments copied",
                format!(
                    "{}{}",
                    report.segments_copied.len(),
                    list(&report.segments_copied)
                ),
            )
            .detail(
                "bytes appended to the last segment",
                report.tail_bytes.to_string(),
            )
            .detail("bytes", report.bytes.to_string());
        if report.lineage_changed {
            note = note.detail(
                "lineage",
                "the store was rolled back in place since the last backup; the backup follows \
                 its new lineage",
            );
        }
        note
    };
    let mut blocks = vec![note.text()];
    let carried = models_carried(&report.models);
    if !carried.is_empty() {
        blocks.push(carried.trim_end().to_string());
    }
    blocks.push(
        Note::new(format!("The backup {target} is read-only"))
            .command(
                "open it read-only in the REPL",
                format!("graphersal --graph {target}"),
            )
            .command(
                "make it the live store",
                format!("graphersal store restore {target}"),
            )
            .text(),
    );
    format!("{}\n", blocks.join("\n\n"))
}

/// The embedding models a copy carries, with their licences (empty without models): who hands
/// the copy on redistributes them. A headline, then one section per model (as `store models`
/// shows them), one `key: value` per line; ends with a newline.
pub fn models_carried(models: &[ModelInfo]) -> String {
    if models.is_empty() {
        return String::new();
    }
    let headline = Note::new(format!(
        "It carries {} embedding model(s) with their licences",
        models.len()
    ))
    .detail("handing it on", "redistributes them")
    .text();
    let models: Vec<(&ModelInfo, bool)> = models
        .iter()
        .map(|model| (model, !model.needed_by.is_empty()))
        .collect();
    format!("{headline}\n\n{}\n", model_sections(&models, &[]))
}

/// Whether a schema's `embedder` names a model a store keeps (a Model2Vec identity), not a
/// built-in embedder such as `hash-ngram/1` (never "missing").
fn stored_kind(identity: &str) -> bool {
    identity.starts_with("model2vec/") || identity.starts_with("model2vec@")
}

/// Whether the schema's identity `named` (`model2vec/<repo>@<12 hex>`, possibly with a
/// `?normalize=` suffix) names the stored model `model` (the same files: the same 12 hex
/// digits, whatever repository name the schema recorded).
pub fn same_model(named: &str, model: &ModelInfo) -> bool {
    let hex = |identity: &str| {
        identity
            .split('?')
            .next()
            .and_then(|base| base.rsplit_once('@'))
            .map(|(_, hex)| hex.to_string())
    };
    hex(named).is_some() && hex(named) == hex(&model.identity)
}

/// A model named in a sentence: its repository and its full name (the identity).
pub fn model_name(model: &ModelInfo) -> String {
    format!("{} ({})", model.repo, model.identity)
}

/// The licence rows of a model: `licence` (the SPDX id, and where the name comes from when the
/// store keeps a standard text for it) and `licence text` (where the text the store keeps comes
/// from). Graphersal never writes a licence: a standard text is the unchanged text of the
/// licence the model card names.
pub fn licence_rows(model: &ModelInfo) -> [(String, String); 2] {
    let licence = if model.license_text == "template" {
        format!("{} (named in the model card)", model.license)
    } else {
        model.license.clone()
    };
    [
        ("licence".to_string(), licence),
        (
            "licence text".to_string(),
            licence_text_value(&model.license_text, &model.license),
        ),
    ]
}

/// The `licence text` value of a model whose licence text comes from `source` (`file`,
/// `template` or `none`) for the licence `licence`: never a file name, never "written by".
fn licence_text_value(source: &str, licence: &str) -> String {
    match source {
        "file" => "from the repository (its own licence file)".to_string(),
        "template" => format!(
            "standard {} text (the repository has no licence file)",
            crate::cli::license_text(licence)
        ),
        _ => "none (no standard text for this licence)".to_string(),
    }
}

/// One model as the store commands show it, one `(key, value)` per line: model, full name,
/// size, licence, licence text, default, used by (`used`: the schema names it), then gated and
/// a problem when there is one.
pub fn model_rows(model: &ModelInfo, used: bool) -> Vec<(String, String)> {
    let mut rows = vec![
        ("model".to_string(), model.repo.clone()),
        ("full name".to_string(), model.identity.clone()),
        ("size".to_string(), bytes_text(model.bytes)),
    ];
    rows.extend(licence_rows(model));
    rows.push((
        "default".to_string(),
        if model.default { "yes" } else { "no" }.to_string(),
    ));
    let used_by = if !model.needed_by.is_empty() {
        format!("the schema ({})", model.needed_by.join(", "))
    } else if used {
        "the schema".to_string()
    } else {
        "nothing (not used by the schema)".to_string()
    };
    rows.push(("used by".to_string(), used_by));
    if model.gated {
        rows.push((
            "gated".to_string(),
            "yes (the terms accepted to download it may restrict passing it on)".to_string(),
        ));
    }
    let problem = match &model.state {
        graphersal::persist::ModelState::Missing => Some("its blob is missing".to_string()),
        state => state.reason().map(str::to_string),
    };
    if let Some(problem) = problem {
        rows.push(("problem".to_string(), problem));
    }
    rows
}

/// The rows of a model the schema names that the store does not keep.
fn missing_model_rows(identity: &str) -> Vec<(String, String)> {
    vec![
        ("full name".to_string(), identity.to_string()),
        (
            "problem".to_string(),
            "the schema names it, the store holds no such model".to_string(),
        ),
    ]
}

/// `graphersal store models <DIR>`: one section per model (`models`, each with whether the
/// schema names it), then one per model the schema names that the store does not keep
/// (`missing`), styled like the sections of the `--server` banner.
pub fn model_sections(models: &[(&ModelInfo, bool)], missing: &[&String]) -> String {
    let mut sections = Vec::new();
    for (model, used) in models {
        let mut section = crate::sections::Section::new("Embedding model");
        section.rows(model_rows(model, *used));
        sections.push(section);
    }
    for identity in missing {
        let mut section = crate::sections::Section::new("Missing embedding model");
        section.rows(missing_model_rows(identity));
        sections.push(section);
    }
    crate::sections::render(&sections)
}

/// `rows` as lines `<indent><key>: <value>` (`colon`) or `<indent><key>  <value>`, the values
/// aligned in one column.
fn rows_text(rows: &[(String, String)], indent: &str, colon: bool) -> String {
    let mark = if colon { ":" } else { " " };
    let width = rows
        .iter()
        .map(|(key, _)| key.chars().count() + 1)
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (key, value) in rows {
        let key = format!("{key}{mark}");
        out.push_str(&format!("{indent}{key:width$} {value}\n"));
    }
    out
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

/// The compaction advice as `store compact-advice` prints it: the verdict as the headline, one
/// `key: value` per line, and the command when compacting is recommended.
pub fn advice_text(advice: &CompactionAdvice, dir: &str) -> String {
    let mut note = Note::new(format!(
        "{}: {}",
        if advice.recommended {
            "compact now"
        } else {
            "no compaction needed"
        },
        advice.reason
    ))
    .detail(
        "store",
        format!(
            "{} ({})",
            bytes_text(advice.total_bytes),
            if advice.compactable {
                "single file"
            } else {
                "directory: compact = prune"
            }
        ),
    );
    if advice.compactable {
        note = note.detail("live", bytes_text(advice.live_bytes)).detail(
            "garbage",
            format!(
                "{} ({:.0} %)",
                bytes_text(advice.garbage_bytes),
                advice.garbage_ratio * 100.0
            ),
        );
    }
    note = note.detail(
        "prune",
        format!(
            "{}{}",
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
        ),
    );
    note = note.detail(
        "reclaimable",
        format!(
            "{} ({:.0} % of the store)",
            bytes_text(advice.reclaimable_bytes),
            advice.reclaimable_ratio * 100.0
        ),
    );
    if advice.compactable {
        note = note
            .detail("disk needed", bytes_text(advice.space_needed))
            .detail(
                "disk free",
                advice.free_space.map_or("unknown".to_string(), bytes_text),
            );
    }
    let lost = &advice.prune.marks_unreachable;
    if !lost.is_empty() {
        note = note.detail(
            "marks lost",
            format!(
                "{} (unreachable afterwards; compact asks for --drop-marks)",
                lost.join(", ")
            ),
        );
    }
    note = note.detail("decided by", advice.decided_by.name());
    if advice.recommended {
        let confirm = if lost.is_empty() { "" } else { " --drop-marks" };
        note = note.command(
            "compact the store now",
            format!("graphersal store compact {dir}{confirm}"),
        );
    }
    format!("{}\n", note.text())
}

/// The stderr note (printed with [`crate::out::print_note`]) of an open that rewrote a
/// missing, damaged or stale `GRAPH` / `GRAPH.copy` from the other copy
/// ([`graphersal::persist::OpenReport::graph_file_repaired`]), or `None` when both copies were
/// intact.
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
                Note::new(format!(
                    "the store {path}: {what}; the copy is left as it is (a read-only open \
                     writes nothing)"
                ))
                .text()
            } else {
                Note::new(format!(
                    "the store {path}: {what}; the other copy was rewritten from it"
                ))
                .command(
                    "check the rest of the store",
                    format!("graphersal store verify {path}"),
                )
                .text()
            }
        })
}

/// Opens a store read-write for a `store` command and prints the repaired-`GRAPH` note.
fn open_rw(path: &str) -> Result<Store, PersistError> {
    let store = Store::open(path, StoreOptions::new())?;
    if let Some(note) = graph_file_repaired_note(path, &store) {
        crate::out::print_note("note", &note);
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
            "Error: cannot create a store in {path}: it is a store already\n{}",
            Note::new(
                "open it, see what it holds, or choose an empty or new directory for the new \
                 store"
            )
            .command("open it", format!("graphersal --graph {path}"))
            .command("see what it holds", format!("graphersal store info {path}"))
            .help()
        ));
    }
    let empty = std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false);
    (!empty).then(|| {
        format!(
            "Error: cannot create a store in {path}: the directory is not empty\n{}",
            Note::new("choose an empty or new directory, or open the existing store if it is one")
                .command(
                    "see whether it is a store and what it holds",
                    format!("graphersal store info {path}")
                )
                .help()
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
         from a backup that holds older snapshots). Nothing was changed.\n{}",
        Note::new(format!(
            "run it again with --drop-marks to {command} anyway, or back the store up first"
        ))
        .command(
            "back the store up first",
            format!("graphersal store backup {dir} <BACKUP_DIR>")
        )
        .help()
    );
    EXIT_QUERY_ERROR
}

fn fail(err: PersistError) -> i32 {
    errln!("{}", graphersal::error::format_diagnostic(&err));
    EXIT_QUERY_ERROR
}

/// `store journal`: prints the pages of the journal (and, following, new records once a
/// second until the process is interrupted). `last`: only the newest N matching records, read
/// newest first from the end of the journal and printed oldest first.
fn journal_command(
    dir: &str,
    mut query: JournalQuery,
    last: Option<u64>,
    follow: bool,
    json: bool,
) -> i32 {
    if let Some(count) = last {
        let newest = query
            .clone()
            .newest_first()
            .limit(usize::try_from(count).unwrap_or(usize::MAX));
        let page = match Store::journal_dir(dir, &newest) {
            Ok(page) => page,
            Err(err) => return fail(err),
        };
        for note in &page.notes {
            errln!("note: {note}");
        }
        if page.truncated {
            errln!("note: older records are gone");
        }
        for record in page.records.iter().rev() {
            if json {
                outln!("{}", record.to_json());
            } else {
                outln!("{}", record.to_line());
            }
        }
        if !follow {
            return EXIT_OK;
        }
        query = query.after(page.next_after);
    }
    loop {
        let page = match Store::journal_dir(dir, &query) {
            Ok(page) => page,
            Err(err) => return fail(err),
        };
        for note in &page.notes {
            errln!("note: {note}");
        }
        if page.truncated {
            errln!("note: older records are gone");
        }
        for record in &page.records {
            if json {
                outln!("{}", record.to_json());
            } else {
                outln!("{}", record.to_line());
            }
        }
        query = query.after(page.next_after);
        if page.more {
            continue;
        }
        if !follow {
            return EXIT_OK;
        }
        if !crate::out::stdout_open() {
            return EXIT_OK;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn usage(message: &str) -> i32 {
    errln!(
        "{}",
        crate::out::usage_text(message, "graphersal store help")
    );
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
                    | "level"
                    | "since"
                    | "source"
                    | "kind"
                    | "after"
                    | "last"
                    | "limit"
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
        "journal" => &[
            "level", "since", "source", "kind", "after", "last", "limit", "follow", "json",
        ],
        "checkpoint" => &["name"],
        "prune" => &["up-to", "drop-marks"],
        "compact" => &["drop-marks"],
        "fork" | "rollback" => &["at-commit", "at-time", "at-mark", "delete"],
        "repair" => &["to", "drop-damaged-blobs"],
        "restore-blobs" => &["from", "json"],
        "backup" => &["zip", "full", "from-memory", "without-models"],
        "export" => &["commit", "without-models"],
        "models" => &["none"],
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
            // A `.gsnap` makes the store WITHOUT opening it: one of an earlier release may name
            // a model it does not carry (the store opens once it is imported), and the report
            // says what still needs a decision (as `store upgrade` does).
            if let Some(source) =
                flag("from").filter(|s| s.to_ascii_lowercase().ends_with(".gsnap"))
            {
                let file = match std::fs::File::open(&source) {
                    Ok(file) => file,
                    Err(err) => {
                        errln!("Error: cannot open '{source}': {err}");
                        return EXIT_USAGE_ERROR;
                    }
                };
                let created =
                    Store::create_from_packed_dir(&backend, std::io::BufReader::new(file), options)
                        .and_then(|report| Ok((Store::inspect(&backend)?, report)));
                return match created {
                    Ok((info, report)) => {
                        print_block(&created_note(&info, dir).text());
                        print_upgrade_models(&report, dir);
                        print_block(&use_it(Note::new(format!("Open {dir}")), dir).text());
                        EXIT_OK
                    }
                    Err(err) => fail(err),
                };
            }
            let created = match flag("from") {
                None => Store::create(&backend, options),
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
                // A graph file of an earlier release may hold saved queries calling a removed
                // function.
                let removed = store
                    .graph()
                    .read_checked()
                    .and_then(|graph| {
                        use graphersal::storage::GraphStorage;
                        Ok(graph
                            .definitions()?
                            .filter(|d| d.kind() == graphersal::catalog::DefinitionKind::QUERY)
                            .cloned()
                            .collect::<Vec<_>>())
                    })
                    .map(|definitions| {
                        graphersal::script::saved_queries_using_removed(&definitions)
                    })
                    .map_err(graphersal::persist::PersistError::Graph);
                store.close()?;
                info.and_then(|info| Ok((info, removed?)))
            }) {
                Ok((info, removed)) => {
                    print_removed_function_uses(&removed, dir);
                    print_block(&use_it(created_note(&info, dir), dir).text());
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
                        outln!("backup      up to commit {}", marker.commit_seq);
                        outln!("  taken     {}", format_time(marker.taken_at));
                        outln!("  access    read-only");
                        outln!("            # make it the live store");
                        outln!("            graphersal store restore {}", positional[0]);
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
                    if info.blobs.count > 0 {
                        outln!(
                            "blobs       {} ({})",
                            info.blobs.count,
                            bytes_text(info.blobs.bytes)
                        );
                        if info.blobs.unreferenced_counted {
                            outln!(
                                "  in use    {}",
                                info.blobs.count.saturating_sub(info.blobs.unreferenced)
                            );
                            outln!(
                                "  unused    {} (the next prune removes unused blobs)",
                                info.blobs.unreferenced
                            );
                        } else {
                            outln!("  in use    not counted (the references could not be read)");
                        }
                    }
                    if !info.models.is_empty() || !info.schema_models.is_empty() {
                        outln!(
                            "models      {} ({}; every copy carries them)",
                            info.models.len(),
                            bytes_text(info.models.iter().map(|m| m.bytes).sum())
                        );
                        // One block per model, one field per line (like the fixed parameters
                        // below).
                        for model in &info.models {
                            let in_use = info.schema_models.iter().any(|u| same_model(u, model));
                            let mut rows = model_rows(model, in_use);
                            let (_, repo) = rows.remove(0);
                            outln!("  {repo}");
                            out!("{}", rows_text(&rows, "    ", false));
                        }
                        for identity in &info.schema_models {
                            if stored_kind(identity)
                                && !info.models.iter().any(|m| same_model(identity, m))
                            {
                                outln!("  missing model");
                                out!(
                                    "{}",
                                    rows_text(&missing_model_rows(identity), "    ", false)
                                );
                            }
                        }
                    }
                    match info.space {
                        Some(space) => {
                            outln!("file        single file, {}", bytes_text(space.total_bytes));
                            outln!("  live      {}", bytes_text(space.live_bytes));
                            outln!(
                                "  free      {} inside the file",
                                bytes_text(space.garbage_bytes())
                            );
                            outln!("            # see whether compacting gives that space back");
                            outln!(
                                "            graphersal store compact-advice {}",
                                positional[0]
                            );
                        }
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
            if positional.first().map(String::as_str) == Some("default") {
                let none = flag("none").is_some();
                let repo = match (positional.len(), none) {
                    (3, false) => Some(positional[2].as_str()),
                    (2, true) => None,
                    _ => {
                        return usage(
                            "`store models default` takes the store and the model's repository, \
                             or the store and --none",
                        );
                    }
                };
                return match Store::set_default_model_dir(
                    positional[1].as_str(),
                    repo,
                    ClosedCommitOptions::new().with_principal("cli"),
                ) {
                    Ok(report) => {
                        match (&report.model, repo) {
                            (Some(model), Some(_)) => {
                                outln!("The default model is now {}.", model_name(model))
                            }
                            _ => outln!(
                                "No model is the default now: embed() without a name uses \
                                 hash-ngram/1."
                            ),
                        }
                        EXIT_OK
                    }
                    Err(err) => fail(err),
                };
            }
            if flag("none").is_some() {
                return usage("--none belongs to `store models default`");
            }
            if positional.first().map(String::as_str) == Some("remove") {
                if positional.len() != 3 {
                    return usage(
                        "`store models remove` takes the store and the model's repository",
                    );
                }
                return match Store::remove_model_dir(
                    positional[1].as_str(),
                    &positional[2],
                    ClosedCommitOptions::new().with_principal("cli"),
                ) {
                    Ok(report) => {
                        if let Some(model) = &report.model {
                            outln!("Removed {}.", model_name(model));
                        }
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
                    crate::out::print_note(
                        "note",
                        &Note::new(format!("{} keeps no embedding models", positional[0]))
                            .command(
                                "download a model into the store",
                                format!(
                                    "graphersal embed-model download <REPO> --into {}",
                                    positional[0]
                                ),
                            )
                            .text(),
                    );
                    let missing: Vec<&String> = used.iter().filter(|i| stored_kind(i)).collect();
                    if !missing.is_empty() {
                        outln!("{}", model_sections(&[], &missing));
                    }
                    EXIT_OK
                }
                Ok(models) => {
                    let missing: Vec<&String> = used
                        .iter()
                        .filter(|identity| {
                            stored_kind(identity) && !models.iter().any(|m| same_model(identity, m))
                        })
                        .collect();
                    let models: Vec<(&ModelInfo, bool)> = models
                        .iter()
                        .map(|model| (model, used.iter().any(|u| same_model(u, model))))
                        .collect();
                    outln!("{}", model_sections(&models, &missing));
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
                    let note = Note::new(format!("Snapshot at commit {}", info.commit_seq))
                        .detail("vertices", info.vertex_count.to_string())
                        .detail("edges", info.edge_count.to_string());
                    let note = if !report.written {
                        note.detail(
                            "written",
                            "no: nothing was committed after it, no new snapshot written",
                        )
                    } else if report.merged {
                        note.detail(
                            "merged from",
                            format!(
                                "the snapshot at commit {} and {} commits",
                                report.base_commit_seq, report.commits_merged
                            ),
                        )
                        .detail(
                            "segment files",
                            format!(
                                "{} reused, {} copied, {} written",
                                report.segments_reused,
                                report.segments_copied,
                                report.segments_written
                            ),
                        )
                    } else {
                        note.detail(
                            "encoded from",
                            "the loaded graph (the WAL since the last snapshot is larger than \
                             the merge bound)",
                        )
                    };
                    print_block(&note.text());
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "verify" => {
            try_code!(need(1));
            match Store::verify_dir(&positional[0]) {
                Ok(report) => {
                    print_block(
                        &Note::new(format!("Checked the store {}", positional[0]))
                            .detail("snapshots", report.snapshots_checked.to_string())
                            .detail("WAL segments", report.segments_checked.to_string())
                            .detail("records", report.records_checked.to_string())
                            .detail(
                                "commits",
                                format!("{}..{}", report.first_commit, report.last_commit),
                            )
                            .text(),
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
                        print_block("ok: no damage found");
                        EXIT_OK
                    } else {
                        if let Ok(damage) = Store::damage_report_dir(&positional[0])
                            && !damage.is_empty()
                        {
                            out!("{damage}");
                        }
                        errln!(
                            "Error: {} problem(s) found\n{}",
                            report.problems.len(),
                            Note::new(
                                "keep the store's files (do not prune); build a repaired copy, \
                                 restore a backup, or fork a target before the damage"
                            )
                            .command(
                                "build a repaired copy in a new directory",
                                format!("graphersal store repair {} --to <NEW_DIR>", positional[0])
                            )
                            .help()
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
                                "Error: the store's GRAPH file cannot be read ({err})\n{}",
                                Note::new(
                                    "keep the store's files (do not prune); build a repaired \
                                     copy, or restore a backup"
                                )
                                .command(
                                    "build a repaired copy in a new directory",
                                    format!(
                                        "graphersal store repair {} --to <NEW_DIR>",
                                        positional[0]
                                    )
                                )
                                .help()
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
                    print_block(
                        &prune_details(Note::new(format!("Pruned {}", positional[0])), &report)
                            .text(),
                    );
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
                    let note =
                        Note::new(format!("Forked {} into {}", positional[0], positional[1]))
                            .detail("at commit", info.commit_seq.to_string())
                            .detail("graph", uuid_text(info.graph_id))
                            .detail("parent", uuid_text(info.parent_graph_id));
                    let note = marks_not_carried(note, &report.marks_not_carried, &positional[0])
                        .detail(
                            "undo",
                            format!(
                                "delete the directory {} ({} is unchanged)",
                                positional[1], positional[0]
                            ),
                        );
                    print_block(&use_it(note, &positional[1]).text());
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
                        print_block(
                            &Note::new(format!("Full backup into {target}"))
                                .detail(
                                    "snapshot",
                                    format!("commit {}", report.snapshot_commit_seq),
                                )
                                .detail("WAL", format!("up to commit {}", report.commit_seq))
                                .detail("files", report.files.len().to_string())
                                .detail("bytes", report.bytes.to_string())
                                .detail("checked", "every checksum read back")
                                .command(
                                    "unpack it into a new live store",
                                    format!("graphersal store restore {target} <NEW_DIR>"),
                                )
                                .text(),
                        );
                        let carried = models_carried(&report.models);
                        if !carried.is_empty() {
                            print_block(carried.trim_end());
                        }
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
                    print_block(backup_summary(&report, target).trim_end());
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
                    return usage(
                        &Note::new(format!("'{dir}' is a file"))
                            .command(
                                "unpack a ZIP backup into a new live store",
                                format!("graphersal store restore {dir} <NEW_DIR>"),
                            )
                            .text(),
                    );
                }
                return match Store::restore_backup(dir) {
                    Ok(marker) => {
                        let note = Note::new(format!("Restored {dir}: it is the live store now"))
                            .detail("backup", format!("up to commit {}", marker.commit_seq))
                            .detail("taken", format_time(marker.taken_at))
                            .detail("graph id", "the same");
                        print_block(&use_it(note, dir).text());
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
                    let note = Note::new(format!(
                        "Restored {} into {}: it is the live store",
                        positional[0], positional[1]
                    ));
                    print_block(&use_it(note, &positional[1]).text());
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
                    let carried = models_carried(&carried);
                    if !carried.is_empty() {
                        outln!();
                        out!("{carried}");
                    }
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
                    let dir = &positional[0];
                    let note = Note::new(format!(
                        "Rolled back {dir} from commit {} to commit {}",
                        report.previous_commit_seq, report.target
                    ))
                    .detail("new lineage", uuid_text(report.graph_id))
                    .detail(
                        "history after it",
                        format!(
                            "commits {}..={}, {} snapshot(s), {} WAL file(s)",
                            report.target + 1,
                            report.previous_commit_seq,
                            report.snapshots_moved.len(),
                            report.segments_moved
                        ),
                    );
                    let note = match &report.attic {
                        Some(id) => note
                            .detail("kept", format!("in the attic as {id}"))
                            .command(
                                "undo (only while nothing new is committed or marked)",
                                format!("graphersal store attic {dir} restore {id}"),
                            )
                            .command(
                                "or keep it as a new store",
                                format!("graphersal store attic {dir} fork {id} <NEW_DIR>"),
                            )
                            .command(
                                "or delete it",
                                format!("graphersal store attic {dir} remove {id}"),
                            ),
                        None => note.detail("kept", "no: deleted, this cannot be undone"),
                    };
                    print_block(&note.text());
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
                    let note = Note::new(format!(
                        "Copied the store {from} into the {} {to}",
                        if matches!(target.space_usage(), Ok(Some(_))) {
                            "single file"
                        } else {
                            "directory"
                        }
                    ))
                    .detail("files", files.to_string())
                    .detail("size", bytes_text(bytes))
                    .detail("verified", "yes")
                    .detail("source", format!("{from} is unchanged"));
                    print_block(&use_it(note, to).text());
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
                    let note = prune_details(Note::new(format!("Compacted {dir}")), &report.prune);
                    let note = match report.compaction {
                        Some(c) => note.detail(
                            "file",
                            format!(
                                "{} -> {} ({} given back)",
                                bytes_text(c.bytes_before),
                                bytes_text(c.bytes_after),
                                bytes_text(report.bytes_freed())
                            ),
                        ),
                        None => note.detail(
                            "file",
                            "a directory store: the prune gave the space back (nothing to \
                             compact)",
                        ),
                    };
                    print_block(&note.text());
                    EXIT_OK
                }
                Err(err) => fail(err),
            }
        }
        "journal" => {
            try_code!(need(1));
            let mut query = JournalQuery::new();
            if let Some(raw) = flag("level") {
                match Level::from_name(&raw) {
                    Some(level) => query = query.min_level(level),
                    None => {
                        return usage(&format!(
                            "--level expects error, warn, notice or info, not {raw:?}"
                        ));
                    }
                }
            }
            if let Some(raw) = flag("since") {
                match parse_time(&raw) {
                    Ok(time) => query = query.since(time),
                    Err(err) => return usage(&format!("--since: {err}")),
                }
            }
            if let Some(source) = flag("source") {
                query = query.source_prefix(source);
            }
            if let Some(kind) = flag("kind") {
                query = query.kind(kind);
            }
            if let Some(after) = try_code!(number("after")) {
                query = query.after(after);
            }
            if let Some(limit) = try_code!(number("limit")) {
                query = query.limit(limit as usize);
            }
            let last = try_code!(number("last"));
            if last.is_some() && flag("after").is_some() {
                return usage("--last and --after exclude each other");
            }
            if last == Some(0) {
                return usage("--last expects a positive number of records");
            }
            journal_command(
                &positional[0],
                query,
                last,
                flag("follow").is_some(),
                flag("json").is_some(),
            )
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
            let options =
                RepairOptions::new().with_drop_damaged_blobs(flag("drop-damaged-blobs").is_some());
            match Store::repair_dir_with(&positional[0], &to, options) {
                Ok(report) => {
                    out!("{}", report.damage);
                    out!("{report}");
                    for blob in &report.blobs_dropped {
                        errln!("note: left out the missing or damaged blob {blob}");
                    }
                    if !report.definitions_dropped.is_empty() {
                        errln!(
                            "note: left out the definitions referencing them: {}",
                            report.definitions_dropped.join(", ")
                        );
                    }
                    if report.is_lossless() {
                        outln!();
                        print_block(&use_it(Note::new("ok: nothing lost"), &to).text());
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
        "restore-blobs" => {
            try_code!(need(1));
            let Some(from) = flag("from").filter(|f| !f.is_empty()) else {
                return usage("`store restore-blobs` needs --from <SOURCE>");
            };
            let source = match blob_source(&from) {
                Ok(source) => source,
                Err(message) => {
                    errln!("Error: {message}");
                    return EXIT_USAGE_ERROR;
                }
            };
            match Store::restore_blobs_from(positional[0].as_str(), source) {
                Ok(report) => {
                    if flag("json").is_some() {
                        outln!("{}", restore_json(&report));
                    } else {
                        let mut note = Note::new(format!(
                            "Restored the blobs of {} from {}",
                            positional[0], report.source
                        ))
                        .detail("referenced", report.referenced.to_string())
                        .detail("missing or damaged", report.wanted.len().to_string())
                        .detail("restored", report.restored.len().to_string())
                        .detail("bytes restored", report.bytes_restored.to_string());
                        for blob in &report.restored {
                            note = note.detail("restored blob", blob);
                        }
                        for blob in &report.not_found {
                            note = note.detail("not in the source", blob);
                        }
                        for blob in &report.damaged_in_source {
                            note = note.detail("damaged there too", blob);
                        }
                        print_block(&note.text());
                    }
                    if report.is_complete() {
                        EXIT_OK
                    } else {
                        errln!(
                            "Error: {} blob(s) are still missing or damaged\n{}",
                            report.not_found.len() + report.damaged_in_source.len(),
                            Note::new(
                                "restore them from another backup or copy of the store, or build \
                                 a copy without them"
                            )
                            .command(
                                "build a repaired copy without them",
                                format!(
                                    "graphersal store repair {} --to <NEW_DIR> --drop-damaged-blobs",
                                    positional[0]
                                )
                            )
                            .help()
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

/// The blob source `store restore-blobs --from` names: a ZIP backup (`.zip`), a packed snapshot
/// (`.gsnap`), else a store or backup (a directory or a single file).
fn blob_source(from: &str) -> Result<BlobSource, String> {
    let lower = from.to_ascii_lowercase();
    let open = || {
        std::fs::File::open(from)
            .map(std::io::BufReader::new)
            .map_err(|err| format!("cannot open '{from}': {err}"))
    };
    if lower.ends_with(".zip") {
        return Ok(BlobSource::zip(open()?, from));
    }
    if lower.ends_with(".gsnap") {
        return Ok(BlobSource::packed(open()?, from));
    }
    if !Path::new(from).exists() {
        return Err(format!("'{from}' does not exist"));
    }
    Ok(BlobSource::store(store_dir_at(from)))
}

/// The report of `store restore-blobs --json`.
fn restore_json(report: &RestoreBlobsReport) -> String {
    let list = |items: &[String]| serde_json::Value::from(items.to_vec());
    serde_json::json!({
        "source": report.source,
        "referenced": report.referenced,
        "wanted": list(&report.wanted),
        "restored": list(&report.restored),
        "bytesRestored": report.bytes_restored,
        "notFound": list(&report.not_found),
        "damagedInSource": list(&report.damaged_in_source),
        "complete": report.is_complete(),
    })
    .to_string()
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
    print_block(
        &Note::new(format!("Verified the {what} {dir}"))
            .detail("store format version", report.from_version.to_string())
            .detail("snapshots", verify.snapshots_checked.to_string())
            .detail("WAL segments", verify.segments_checked.to_string())
            .detail("records", verify.records_checked.to_string())
            .text(),
    );
    if report.in_place {
        let kept = if report.migration_commit.is_some() || !report.models_converted.is_empty() {
            "kept, the models/ directory migrated (below)"
        } else {
            "kept"
        };
        print_block(
            &Note::new(format!(
                "Upgraded {dir} in place to store format version {}",
                report.to_version
            ))
            .detail("GRAPH", "rewritten")
            .detail("every other file", kept)
            .detail("the older Graphersal", "no longer opens it")
            .text(),
        );
    } else {
        let to = &positional[1];
        print_block(
            &Note::new(format!("Upgraded {dir} into {to}"))
                .detail("store format version", report.to_version.to_string())
                .detail("files", report.files_copied.to_string())
                .detail("size", bytes_text(report.bytes_copied))
                .detail("verified", "yes")
                .detail(
                    "source",
                    format!(
                        "{dir} is unchanged (keep using {to}: it is the same store, with the \
                         same identity)"
                    ),
                )
                .text(),
        );
    }
    let target = if report.in_place { dir } else { &positional[1] };
    print_upgrade_models(&report, target);
    if report.backup.is_some() {
        print_block(
            &Note::new(format!("{target} stays a backup"))
                .command(
                    "make it the live store",
                    format!("graphersal store restore {target}"),
                )
                .text(),
        );
    } else {
        print_block(&use_it(Note::new(format!("Open {target}")), target).text());
    }
    EXIT_OK
}

/// What an upgrade did to the embedding models (Graphersal 0.4.0 stores keep them under
/// `models/`; they became blobs with catalog definitions), the models the schema needs that the
/// store lacks (`warning:`) and the vector properties without a rule (`note:`, with the ready
/// schema patch). The exit code does not change.
fn print_upgrade_models(report: &graphersal::persist::UpgradeReport, target: &str) {
    for model in &report.models_converted {
        print_block(
            &Note::new(format!(
                "Converted the embedding model {} into a blob with its catalog definition",
                model.former_identity()
            ))
            .detail("full name", model.identity())
            .detail("size", bytes_text(model.bytes()))
            .text(),
        );
        if model.renamed() {
            let note = crate::out::Note::new(format!(
                "the model {} had no repository name of its own in the store",
                model.former_identity()
            ))
            .detail("now", model.identity())
            .detail("repository", model.repo())
            .detail(
                "schema",
                "its embedder naming the model was renamed with it",
            )
            .command(
                "give it its real name: import its files again under that name, then point \
                 the schema at it",
                format!("graphersal embed-model import <dir> --into {target} --repo <owner/name>"),
            );
            crate::out::print_note("note", &note.text());
        }
    }
    if let Some(commit) = report.migration_commit {
        let note = Note::new(format!(
            "The models were defined by commit {commit}; the history before it has no models."
        ));
        let note = if report.backup.is_some() {
            note.command(
                "back the upgraded store up into a new directory: this backup does not take \
                 increments of the store upgraded on its own (each has its own migration commit)",
                "graphersal store backup <store> <new_dir>",
            )
        } else {
            note.command(
                "take a full backup now: a backup taken before the upgrade does not take \
                 increments of the upgraded store",
                format!("graphersal store backup {target} <new_dir>"),
            )
        };
        print_block(&note.text());
    }
    for missing in &report.models_missing {
        crate::out::print_note("warning", &missing.note(target));
    }
    // One note per model (its properties with one combined patch), then the saved queries
    // writing a vector property directly.
    for note in report.vector_notes(target) {
        crate::out::print_note("note", &note);
    }
    print_removed_function_uses(&report.saved_queries_using_removed, target);
}

/// The block `store create` prints for the new store at `dir`.
fn created_note(info: &graphersal::persist::StoreInfo, dir: &str) -> Note {
    Note::new(format!(
        "Created the {} {dir}",
        if info.space.is_some() {
            "single-file store"
        } else {
            "store"
        }
    ))
    .detail("graph", uuid_text(info.graph_id))
    .detail("commit", info.commit_seq.to_string())
    .detail("snapshots", info.snapshots.len().to_string())
    .detail(
        "damage policy",
        format!("{} (fixed for its life)", info.damage_policy),
    )
}

/// `note` with what a prune did: the commit it kept from, the snapshots, WAL segments and
/// blobs it removed, the marks it made unreachable.
fn prune_details(note: Note, report: &graphersal::persist::PruneReport) -> Note {
    let mut note = note
        .detail("kept from", format!("commit {}", report.kept_from))
        .detail(
            "snapshots removed",
            format!(
                "{}{}",
                report.snapshots_removed.len(),
                commit_list(&report.snapshots_removed)
            ),
        )
        .detail(
            "WAL segments removed",
            report.segments_removed.len().to_string(),
        );
    if !report.marks_unreachable.is_empty() {
        note = note.detail(
            "marks no longer reachable",
            format!(
                "{} (their history was pruned)",
                report.marks_unreachable.join(", ")
            ),
        );
    }
    if report.blobs_removed > 0 {
        note = note.detail(
            "unreferenced blobs removed",
            format!(
                "{} ({})",
                report.blobs_removed,
                bytes_text(report.blob_bytes_removed)
            ),
        );
    }
    note
}

/// The saved queries calling a DSL function this version removed (`note:` lines with the
/// replacement); a saved query is never rewritten.
fn print_removed_function_uses(uses: &[graphersal::catalog::RemovedFunctionUse], store: &str) {
    for removed in uses {
        crate::out::print_note("note", &removed.note(store));
    }
}

/// `note` with the marks a fork did not carry (their number, where they stay usable, then one
/// `mark: name @ commit N` line each).
fn marks_not_carried(note: Note, marks: &[graphersal::changes::Mark], source: &str) -> Note {
    if marks.is_empty() {
        return note;
    }
    let note = note.detail(
        "marks not carried",
        format!("{} (they stay targets of {source} only)", marks.len()),
    );
    marks.iter().fold(note, |note, mark| {
        note.detail(
            "mark",
            format!("{} @ commit {}", mark.name(), mark.commit_seq()),
        )
    })
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
        usage(
            &Note::new(format!("`store attic <DIR> {action}` takes an entry id"))
                .command("list the entries", format!("graphersal store attic {dir}"))
                .text(),
        )
    };
    match action {
        "list" => match Store::attic_list_dir(dir) {
            Ok(entries) => {
                if entries.is_empty() {
                    outln!("The attic is empty.");
                    return EXIT_OK;
                }
                // One section per entry, one field per line (as `store models`).
                let sections: Vec<crate::sections::Section> = entries
                    .iter()
                    .map(|e| {
                        let mut section = crate::sections::Section::new("Attic entry");
                        section.unwrapped("id", e.id.clone());
                        section.row(
                            "commits",
                            format!("{}..={}", e.target + 1, e.last_commit_seq),
                        );
                        section.row("rolled back", format_time(e.time));
                        section.row("reason", e.reason.to_string());
                        section.row("size", bytes_text(e.bytes));
                        section.row("snapshots", e.snapshots.len().to_string());
                        section
                    })
                    .collect();
                outln!("{}", crate::sections::render(&sections));
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
                    let note = Note::new(format!("Forked the attic entry {id} into {new_dir}"))
                        .detail("at commit", info.commit_seq.to_string())
                        .detail("graph", uuid_text(info.graph_id))
                        .detail("parent", uuid_text(info.parent_graph_id));
                    let note = marks_not_carried(note, &report.marks_not_carried, dir)
                        .detail("attic entry", "stays")
                        .detail("undo", format!("delete the directory {new_dir}"));
                    print_block(&use_it(note, new_dir).text());
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
    fn the_licence_text_says_where_the_text_comes_from_without_a_file_name() {
        assert_eq!(
            licence_text_value("file", "apache-2.0"),
            "from the repository (its own licence file)"
        );
        assert_eq!(
            licence_text_value("template", "mit"),
            "standard MIT text (the repository has no licence file)"
        );
        assert_eq!(
            licence_text_value("none", "other"),
            "none (no standard text for this licence)"
        );
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
            "restore-blobs",
            "convert",
            "upgrade",
            "compact",
            "compact-advice",
            "journal",
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
