//! The declared events of the system journal (book page "System Journal", generated catalogue):
//! every record the journal writes has one of these kinds, with its source, level, description
//! and fields, like an error code. A package that adds operations declares its events here.
// The descriptions are read by the generated book catalogue (a unit test).
#![cfg_attr(not(test), allow(dead_code))]

use super::Level;

/// One field of an event kind.
pub(crate) struct FieldDecl {
    /// The field name, as it appears in a record.
    pub(crate) name: &'static str,
    /// What it holds.
    pub(crate) description: &'static str,
    /// Whether a record may leave it out.
    pub(crate) optional: bool,
}

/// Whether and how repeats of an event are collapsed (format spec 4.7, book "Repeats and
/// states").
pub(crate) enum Collapse {
    /// Every occurrence is written.
    Never,
    /// A record whose `kind`, `source` and these fields' values equal one of the last five
    /// written records is counted instead of written (an empty list: the kind alone).
    By(&'static [&'static str]),
}

/// The form of an event kind.
pub(crate) enum Form {
    /// A single event.
    Event(Collapse),
    /// A state that holds from this record until its `cleared` kind: written once, repeats
    /// collapsed while it holds, a `journal.state_reminder` once per window. `key` names the
    /// fields that tell two states of the kind apart.
    State {
        cleared: &'static str,
        key: &'static [&'static str],
    },
    /// The end of the state `state`.
    Cleared { state: &'static str },
}

/// A declared event kind.
pub(crate) struct EventKind {
    /// The stable code (`store.backup_done`).
    pub(crate) code: &'static str,
    /// The source hierarchy (`store.backup`).
    pub(crate) source: &'static str,
    /// The level; an event may override it (`journal.repeated` takes its original's).
    pub(crate) level: Level,
    /// One sentence for the catalogue.
    pub(crate) description: &'static str,
    /// The fields a record of this kind may hold.
    pub(crate) fields: &'static [FieldDecl],
    /// Whether it is an event or a state, and its collapse key.
    pub(crate) form: Form,
    /// The package that will emit it when nothing does yet (`None`: emitted today).
    pub(crate) emitted_later_by: Option<&'static str>,
}

const fn f(name: &'static str, description: &'static str) -> FieldDecl {
    FieldDecl {
        name,
        description,
        optional: false,
    }
}

const fn opt(name: &'static str, description: &'static str) -> FieldDecl {
    FieldDecl {
        name,
        description,
        optional: true,
    }
}

const fn event(
    code: &'static str,
    source: &'static str,
    level: Level,
    description: &'static str,
    fields: &'static [FieldDecl],
    collapse: Collapse,
) -> EventKind {
    EventKind {
        code,
        source,
        level,
        description,
        fields,
        form: Form::Event(collapse),
        emitted_later_by: None,
    }
}

pub(crate) static REPEATED: EventKind = event(
    "journal.repeated",
    "journal",
    Level::Info,
    "Closes a group of repeats: the record `of_origin_seq` occurred `count` more times between \
     `first_us` and `last_us` (written when the original leaves the last five records, after an \
     hour, or at close). Its level is the original's.",
    &[
        f("of_origin_seq", "the origin_seq of the original record"),
        f("of_kind", "the kind of the original record"),
        f("count", "how many more times it occurred"),
        f("first_us", "the first repeat (µs since the Unix epoch)"),
        f("last_us", "the last repeat (µs since the Unix epoch)"),
    ],
    Collapse::Never,
);

pub(crate) static STATE_REMINDER: EventKind = event(
    "journal.state_reminder",
    "journal",
    Level::Info,
    "A state still holds (once per hour while it does), with its current values. Its level is \
     the state's.",
    &[
        f("state", "the kind of the state"),
        f("since_us", "when the state began (µs since the Unix epoch)"),
        f(
            "count",
            "how many times it was reported again since the last record",
        ),
    ],
    Collapse::By(&["state"]),
);

pub(crate) static RECORDS_LOST: EventKind = event(
    "journal.records_lost",
    "journal",
    Level::Warn,
    "Writing journal records failed; this many records are missing from the stored journal \
     (they were kept in memory only).",
    &[f("count", "the records not written")],
    Collapse::By(&[]),
);

pub(crate) static CLOCK_STEP_BACK: EventKind = event(
    "clock.step_back",
    "clock",
    Level::Warn,
    "The system clock stepped back more than a second behind the instance clock; times stay at \
     the instance clock until the system clock catches up.",
    &[
        f("system_us", "the system clock's reading (µs)"),
        f("floor_us", "the instance clock (µs)"),
        f("behind_ms", "how far the system clock is behind (ms)"),
    ],
    Collapse::By(&[]),
);

pub(crate) static STORE_OPENED: EventKind = event(
    "store.opened",
    "store",
    Level::Info,
    "The store was opened.",
    &[
        f("commit_seq", "the position after the open"),
        f("replayed", "WAL commits replayed onto the snapshot"),
        f("load_ms", "how long the open took (ms)"),
        f(
            "read_only",
            "whether it opened read-only (a backup, maintenance mode, ...)",
        ),
    ],
    Collapse::Never,
);

pub(crate) static STORE_RECOVERED: EventKind = event(
    "store.recovered",
    "store.recovery",
    Level::Notice,
    "The open repaired the traces of a crash or an interrupted operation.",
    &[
        f(
            "torn_tail_cut_bytes",
            "bytes of an incomplete last WAL record cut off",
        ),
        f(
            "intent_completed",
            "the interrupted operation completed (empty: none)",
        ),
        f("tmp_removed", "temporary files removed"),
        f("wal_cut", "whether a refused WAL record was cut off"),
    ],
    Collapse::Never,
);

pub(crate) static STORE_CLOSED: EventKind = event(
    "store.closed",
    "store",
    Level::Info,
    "The store was closed.",
    &[
        f("commit_seq", "the last commit"),
        f("clean", "whether it was closed cleanly"),
    ],
    Collapse::Never,
);

pub(crate) static STORE_READ_ONLY: EventKind = EventKind {
    code: "store.read_only",
    source: "store",
    level: Level::Warn,
    description: "The store is read-only: it holds something critical this version does not \
                  know.",
    fields: &[f("reason", "why")],
    form: Form::State {
        cleared: "store.read_only_cleared",
        key: &[],
    },
    emitted_later_by: None,
};

pub(crate) static STORE_READ_ONLY_CLEARED: EventKind = EventKind {
    code: "store.read_only_cleared",
    source: "store",
    level: Level::Notice,
    description: "The store is writable again.",
    fields: &[],
    form: Form::Cleared {
        state: "store.read_only",
    },
    emitted_later_by: Some("a later version (a store turns writable only at a new open today)"),
};

pub(crate) static CHECKPOINT_DONE: EventKind = event(
    "store.checkpoint_done",
    "store.checkpoint",
    Level::Info,
    "A checkpoint wrote a snapshot.",
    &[
        f("commit_seq", "the snapshot's position"),
        f(
            "merged",
            "whether it was merged from the last snapshot and the WAL",
        ),
        f("bytes", "the snapshot's size"),
        f("ms", "how long it took"),
        f("automatic", "whether the store started it itself"),
    ],
    Collapse::Never,
);

pub(crate) static CHECKPOINT_FAILED: EventKind = event(
    "store.checkpoint_failed",
    "store.checkpoint",
    Level::Error,
    "A checkpoint failed; nothing visible was written (level warn when the memory ceiling \
     refused it).",
    &[
        f("error", "the error"),
        f("automatic", "whether the store started it itself"),
    ],
    Collapse::By(&[]),
);

pub(crate) static BACKUP_DONE: EventKind = event(
    "store.backup_done",
    "store.backup",
    Level::Notice,
    "A backup was written and verified.",
    &[
        f("target", "where it went"),
        f("mode", "full, increment, zip or memory"),
        f("commit_seq", "the backup's position"),
        f("bytes", "the bytes written"),
    ],
    Collapse::Never,
);

pub(crate) static BACKUP_FAILED: EventKind = event(
    "store.backup_failed",
    "store.backup",
    Level::Error,
    "A backup failed (level warn when the memory ceiling refused it).",
    &[
        f("target", "where it should have gone"),
        f("side", "where damage was found: store, backup or none"),
        f("error", "the error"),
    ],
    Collapse::By(&["target"]),
);

pub(crate) static PRUNE_DONE: EventKind = event(
    "store.prune_done",
    "store.prune",
    Level::Notice,
    "Old snapshots and WAL were removed.",
    &[
        f("up_to", "the position pruned up to"),
        f("snapshots_removed", "snapshots removed"),
        f("segments_removed", "WAL segments removed"),
        f("bytes", "bytes freed"),
        f("marks_unreachable", "marks no longer reachable"),
    ],
    Collapse::Never,
);

pub(crate) static DAMAGE_FOUND: EventKind = EventKind {
    code: "store.damage_found",
    source: "store.damage",
    level: Level::Error,
    description: "Damage was found in the store's files while it is open.",
    fields: &[
        f("found_by", "the operation that found it"),
        f("problems", "how many problems"),
        f("first_path", "the first damaged file"),
    ],
    form: Form::State {
        cleared: "store.damage_cleared",
        key: &["first_path"],
    },
    emitted_later_by: None,
};

pub(crate) static DAMAGE_CLEARED: EventKind = EventKind {
    code: "store.damage_cleared",
    source: "store.damage",
    level: Level::Notice,
    description: "Damage found earlier is gone.",
    fields: &[f("first_path", "the file that was damaged")],
    form: Form::Cleared {
        state: "store.damage_found",
    },
    emitted_later_by: Some(
        "a later version (damage is cleared by a repair into a new store today)",
    ),
};

pub(crate) static MAINTENANCE_MODE: EventKind = event(
    "store.maintenance_mode",
    "store.damage",
    Level::Error,
    "The store opened in maintenance mode (read-only): its files are damaged.",
    &[f("trigger", "what the open ran into")],
    Collapse::Never,
);

pub(crate) static REPAIRED: EventKind = event(
    "store.repaired",
    "store.damage",
    Level::Notice,
    "The store's salvaged state was written as a new store.",
    &[
        f("target", "the new store"),
        f("commit_seq", "its position"),
        f(
            "diverged",
            "whether the salvaged state diverged from the WAL",
        ),
    ],
    Collapse::Never,
);

pub(crate) static ROLLED_BACK: EventKind = event(
    "store.rolled_back",
    "store.rollback",
    Level::Notice,
    "The store was rolled back; the later history moved into the attic (or was deleted).",
    &[
        f("target", "the commit rolled back to"),
        f("mode", "move or delete"),
        opt("attic_entry", "the attic entry of the later history"),
    ],
    Collapse::Never,
);

pub(crate) static ATTIC_RESTORED: EventKind = event(
    "store.attic_restored",
    "store.attic",
    Level::Notice,
    "An attic entry was restored as the store's current history.",
    &[f("entry", "the attic entry")],
    Collapse::Never,
);

pub(crate) static ATTIC_REMOVED: EventKind = event(
    "store.attic_removed",
    "store.attic",
    Level::Notice,
    "An attic entry was removed.",
    &[f("entry", "the attic entry")],
    Collapse::Never,
);

pub(crate) static CREATED: EventKind = event(
    "store.created",
    "store",
    Level::Notice,
    "The store was created (the first record of every store).",
    &[
        f("how", "create, packed, fork, attic_fork or repair"),
        opt("from_store_id", "the store it was made from"),
        opt("from_commit", "the position it was made from"),
    ],
    Collapse::Never,
);

pub(crate) static UPGRADED: EventKind = EventKind {
    code: "store.upgraded",
    source: "store.upgrade",
    level: Level::Notice,
    description: "The store was upgraded to this version's store format.",
    fields: &[
        f("from_version", "the old store format version"),
        f("to_version", "the new one"),
        f("in_place", "whether it was upgraded in place"),
        f(
            "models",
            "the embedding models of models/ that became blobs (Graphersal 0.4.0 stores)",
        ),
    ],
    form: Form::Event(Collapse::Never),
    emitted_later_by: None,
};

pub(crate) static VERIFIED: EventKind = event(
    "store.verified",
    "store.verify",
    Level::Info,
    "The store's files were verified (level warn when problems or warnings were found).",
    &[
        f("problems", "damage found"),
        f("warnings", "warnings"),
        f("notes", "notes (the system journal's among them)"),
    ],
    Collapse::Never,
);

pub(crate) static AUTH_DENIED: EventKind = event(
    "auth.denied",
    "auth",
    Level::Warn,
    "The authorizer denied a request (one per execution; no query text).",
    &[
        f("action", "the action asked"),
        f("resource_kind", "the kind of resource"),
        f("channel", "the channel the work came through"),
    ],
    Collapse::By(&["action", "resource_kind", "channel"]),
);

pub(crate) static BLOB_DAMAGED: EventKind = event(
    "blob.damaged",
    "blob",
    Level::Error,
    "A blob is missing or damaged (never store damage: the store keeps working).",
    &[
        f("hash", "the blob's hash (hex)"),
        f("length", "its length in bytes"),
        f("failure", "what failed: missing, length, crc or hash"),
        f("found_by", "the operation that found it"),
        opt(
            "referrers",
            "the catalog definitions that reference it, as kind:name",
        ),
        f("recommendation", "how to get the bytes back"),
    ],
    Collapse::By(&["hash", "failure"]),
);

pub(crate) static BLOB_REPAIRED: EventKind = event(
    "blob.repaired",
    "blob",
    Level::Notice,
    "A damaged blob was replaced by intact bytes of the same hash.",
    &[
        f("hash", "the blob's hash (hex)"),
        f("how", "rewritten (written again) or restored (copied back)"),
        opt(
            "source",
            "where a restored blob came from (graphersal store restore-blobs --from)",
        ),
    ],
    Collapse::Never,
);

pub(crate) static BLOB_DROPPED: EventKind = event(
    "blob.dropped",
    "blob",
    Level::Warn,
    "A repair left out a missing or damaged blob and the catalog definitions referencing it \
     (graphersal store repair --drop-damaged-blobs).",
    &[
        f("hash", "the blob's hash (hex)"),
        f(
            "referrers",
            "the catalog definitions left out with it, as kind:name",
        ),
    ],
    Collapse::Never,
);

pub(crate) static CATALOG_CLOSED_COMMIT: EventKind = event(
    "catalog.closed_commit",
    "catalog",
    Level::Notice,
    "A catalog change was appended to a CLOSED store's WAL as one commit, without loading the \
     graph (format spec 12.12).",
    &[
        f("ops", "the catalog and schema changes of the commit"),
        f("kinds", "the definition kinds it changed"),
        f(
            "wal_tail_commits",
            "the commits after the newest snapshot read to find the current catalog",
        ),
        f("ms", "how long it took"),
    ],
    Collapse::Never,
);

pub(crate) static BLOB_COLLECTED: EventKind = event(
    "blob.collected",
    "blob",
    Level::Info,
    "Blobs no retained history references were removed.",
    &[
        f("count", "blobs removed"),
        f("bytes", "bytes freed"),
        f("by", "prune, attic_remove or rollback_delete"),
    ],
    Collapse::Never,
);

pub(crate) static LEDGER_REBUILT: EventKind = event(
    "ledger.rebuilt",
    "ledger",
    Level::Notice,
    "The derived blob ledger was missing or damaged and was rebuilt from the WAL.",
    &[
        f("frames", "the frames written"),
        f("reason", "missing or damaged"),
    ],
    Collapse::Never,
);

pub(crate) static MODEL_IMPORTED: EventKind = event(
    "model.imported",
    "model",
    Level::Notice,
    "An embedding model was imported into the store (one commit defining it).",
    &[
        f("repo", "its repository"),
        f("identity", "its identity"),
        f("bytes", "the bytes of its blob"),
        f("default", "whether it became the default model"),
        opt(
            "closed",
            "true when the change was committed to a closed store (format spec 12.12)",
        ),
    ],
    Collapse::Never,
);

pub(crate) static MODEL_MIGRATED: EventKind = event(
    "model.migrated",
    "model",
    Level::Notice,
    "An embedding model a Graphersal 0.4.0 store kept under models/ became a blob with a \
     catalog definition (store upgrade to store format version 4; the migration commit).",
    &[
        f(
            "repo",
            "its repository (local/<12 hex> when 0.4.0 kept none)",
        ),
        f("identity", "its identity now"),
        f("former_identity", "its identity in the 0.4.0 store"),
        f("bytes", "the bytes of its blob"),
        opt(
            "closed",
            "true when the change was committed to a closed store (format spec 12.12)",
        ),
    ],
    Collapse::Never,
);

pub(crate) static MODEL_REPLACED: EventKind = event(
    "model.replaced",
    "model",
    Level::Notice,
    "A new revision of an embedding model replaced the current one (one commit).",
    &[
        f("repo", "its repository"),
        f("old_identity", "the replaced revision's identity"),
        f("new_identity", "the new revision's identity"),
        opt(
            "closed",
            "true when the change was committed to a closed store (format spec 12.12)",
        ),
    ],
    Collapse::Never,
);

pub(crate) static MODEL_REMOVED: EventKind = event(
    "model.removed",
    "model",
    Level::Notice,
    "An embedding model was removed from the store's current state (one commit).",
    &[
        f("repo", "its repository"),
        f("identity", "its identity"),
        opt(
            "closed",
            "true when the change was committed to a closed store (format spec 12.12)",
        ),
    ],
    Collapse::Never,
);

pub(crate) static MODEL_DEFAULT_CHANGED: EventKind = event(
    "model.default_changed",
    "model",
    Level::Notice,
    "The default embedding model changed (one commit).",
    &[
        f(
            "from",
            "the repository of the previous default (empty: none)",
        ),
        f("to", "the repository of the new default (empty: none)"),
        opt(
            "closed",
            "true when the change was committed to a closed store (format spec 12.12)",
        ),
    ],
    Collapse::Never,
);

pub(crate) static MODEL_LOADED: EventKind = event(
    "model.loaded",
    "model",
    Level::Info,
    "An embedding model was loaded into memory.",
    &[
        f("repo", "its repository"),
        f("identity", "its identity"),
        f(
            "load_bytes",
            concat!(
                "the memory the loaded model takes, measured at the load (counted by the ",
                "tracking allocator, else the loader's estimate), not its size on disk"
            ),
        ),
        f("ms", "how long the load took"),
    ],
    Collapse::By(&["identity"]),
);

pub(crate) static MODEL_UNLOADED: EventKind = event(
    "model.unloaded",
    "model",
    Level::Info,
    "An embedding model no open position uses was unloaded.",
    &[f("repo", "its repository"), f("identity", "its identity")],
    Collapse::By(&["identity"]),
);

pub(crate) static MODEL_LOAD_FAILED: EventKind = event(
    "model.load_failed",
    "model",
    Level::Error,
    "An embedding model the schema needs did not load: the store did not open.",
    &[
        f("repo", "its repository"),
        f("identity", "its identity"),
        f("cause", "damaged, missing, does_not_fit or invalid"),
        f("needed", "the bytes a load needs (blob and loaded model)"),
        f("recommendation", "how to bring the model back"),
    ],
    Collapse::By(&["identity", "cause"]),
);

pub(crate) static MODEL_UNAVAILABLE: EventKind = event(
    "model.unavailable",
    "model",
    Level::Error,
    "An embedding model nothing needs at open is missing or damaged: the store works, queries \
     with the model fail.",
    &[
        f("repo", "its repository"),
        f("identity", "its identity"),
        f("cause", "damaged, missing, does_not_fit or invalid"),
        f("recommendation", "how to bring the model back"),
    ],
    Collapse::By(&["identity", "cause"]),
);

pub(crate) static VECTOR_FILL_STARTED: EventKind = event(
    "vector_fill.started",
    "vector_fill",
    Level::Notice,
    "A vector fill started under the write lock (a vector rule added or changed, or its model \
     replaced): the whole database is locked until it ends.",
    &[
        f("reason", "rule_added, rule_changed or model_replaced"),
        f("rules", "the rules filled: element, label, from, to, model"),
        f("elements", "the elements to embed"),
        opt(
            "estimate_ms",
            "the estimated time, when the model's speed is known",
        ),
    ],
    Collapse::Never,
);

pub(crate) static VECTOR_FILL_DONE: EventKind = event(
    "vector_fill.done",
    "vector_fill",
    Level::Notice,
    "A vector fill ended with the commit of its unit (`commit`); `op` is its start record.",
    &[
        f("embedded", "the elements embedded"),
        f("removed", "the vectors removed (a source without text)"),
        f("elapsed_ms", "how long the fill held the lock"),
    ],
    Collapse::Never,
);

pub(crate) static VECTOR_FILL_FAILED: EventKind = event(
    "vector_fill.failed",
    "vector_fill",
    Level::Error,
    "A vector fill failed (the model, the memory ceiling, a cancel): its unit was rolled back, \
     nothing was written.",
    &[
        f("cause", "the error"),
        f("elapsed_ms", "how long the fill held the lock"),
        f(
            "embedded_before_failure",
            "the elements embedded before it failed",
        ),
    ],
    Collapse::Never,
);

/// Every declared event kind, in catalogue order.
pub(crate) static EVENT_KINDS: &[&EventKind] = &[
    &REPEATED,
    &STATE_REMINDER,
    &RECORDS_LOST,
    &CLOCK_STEP_BACK,
    &STORE_OPENED,
    &STORE_RECOVERED,
    &STORE_CLOSED,
    &STORE_READ_ONLY,
    &STORE_READ_ONLY_CLEARED,
    &CHECKPOINT_DONE,
    &CHECKPOINT_FAILED,
    &BACKUP_DONE,
    &BACKUP_FAILED,
    &PRUNE_DONE,
    &DAMAGE_FOUND,
    &DAMAGE_CLEARED,
    &MAINTENANCE_MODE,
    &REPAIRED,
    &ROLLED_BACK,
    &ATTIC_RESTORED,
    &ATTIC_REMOVED,
    &CREATED,
    &UPGRADED,
    &VERIFIED,
    &AUTH_DENIED,
    &BLOB_DAMAGED,
    &BLOB_REPAIRED,
    &BLOB_DROPPED,
    &BLOB_COLLECTED,
    &CATALOG_CLOSED_COMMIT,
    &LEDGER_REBUILT,
    &MODEL_IMPORTED,
    &MODEL_MIGRATED,
    &MODEL_REPLACED,
    &MODEL_REMOVED,
    &MODEL_DEFAULT_CHANGED,
    &MODEL_LOADED,
    &MODEL_UNLOADED,
    &MODEL_LOAD_FAILED,
    &MODEL_UNAVAILABLE,
    &VECTOR_FILL_STARTED,
    &VECTOR_FILL_DONE,
    &VECTOR_FILL_FAILED,
];

/// The declared kind with `code`.
pub(crate) fn kind(code: &str) -> Option<&'static EventKind> {
    EVENT_KINDS.iter().copied().find(|kind| kind.code == code)
}

/// The event catalogue as Markdown (the generated block of the book page "System Journal"):
/// one table row per kind.
pub(crate) fn catalogue_markdown() -> String {
    let mut out = String::from(
        "| Kind | Level | Source | Form | Fields | Description |\n|---|---|---|---|---|---|\n",
    );
    for kind in EVENT_KINDS {
        let form = match &kind.form {
            Form::Event(Collapse::Never) => "event".to_string(),
            Form::Event(Collapse::By([])) => "event, collapsed by kind".to_string(),
            Form::Event(Collapse::By(fields)) => {
                format!("event, collapsed by {}", backticked(fields))
            }
            Form::State { cleared, key: [] } => {
                format!("state until `{cleared}`")
            }
            Form::State { cleared, key } => {
                format!("state per {} until `{cleared}`", backticked(key))
            }
            Form::Cleared { state } => format!("ends `{state}`"),
        };
        let fields = kind
            .fields
            .iter()
            .map(|field| {
                format!(
                    "`{}`{}: {}",
                    field.name,
                    if field.optional { "?" } else { "" },
                    field.description
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let description = match kind.emitted_later_by {
            Some(later) => format!("{} Not emitted yet: {later}.", kind.description),
            None => kind.description.to_string(),
        };
        out.push_str(&format!(
            "| `{}` | {} | `{}` | {} | {} | {} |\n",
            kind.code,
            kind.level.name(),
            kind.source,
            form,
            if fields.is_empty() {
                "-".to_string()
            } else {
                fields.replace('|', "\\|")
            },
            description.replace('|', "\\|"),
        ));
    }
    out
}

fn backticked(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|field| format!("`{field}`"))
        .collect::<Vec<_>>()
        .join(", ")
}
