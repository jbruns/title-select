-- Source Set repair (#17): repair plans and their steps, and the kept files repairs make in Source Sets.

-- A Source Set a repair may write to. generation counts approved repairs; each keeps files under its own
-- `.title-select-retained/<generation>/`. marker_at is when its marker was written, by the first approved repair.
CREATE TABLE source_folders (
    source_set_id INTEGER PRIMARY KEY REFERENCES source_sets (id),
    generation INTEGER NOT NULL DEFAULT 0,
    marker_at TEXT
);
-- As ingest_dirs, ingest_temps and ingest_temp_names, for the directories a repair creates in a Source Set.
CREATE TABLE source_dirs (
    folder_id INTEGER NOT NULL REFERENCES source_sets (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE source_temps (
    folder_id INTEGER NOT NULL REFERENCES source_sets (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE source_temp_names (
    folder_id INTEGER NOT NULL REFERENCES source_sets (id),
    name TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- A repair Change Plan: RepairPlanContent JSON and the SHA-256 of that JSON, which an approval binds to. It is carried
-- out when approved; outcomes is a JSON array of RepairOutcome. audit_job_id is the incremental Audit it queued;
-- audit_owed is set while that Audit waits for another Job of the Source Set to end; readers and
-- hashers are the workers it runs with.
CREATE TABLE repair_plans (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approved_digest TEXT,
    approved_by TEXT,
    approved_at TEXT,
    finished_at TEXT,
    outcomes TEXT,
    recovery_error TEXT,
    audit_owed INTEGER NOT NULL DEFAULT 0,
    audit_job_id INTEGER REFERENCES jobs (id),
    audit_error TEXT,
    readers INTEGER NOT NULL,
    hashers INTEGER NOT NULL
);
CREATE INDEX repair_plans_unfinished ON repair_plans (source_set_id) WHERE approved_at IS NOT NULL
    AND finished_at IS NULL;

-- A file a repair kept aside in its Source Set, at `.title-select-retained/<generation>/<kept_name>`; name is where
-- it was, relative to the Source Set's folder.
CREATE TABLE source_kept (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    plan_id INTEGER NOT NULL REFERENCES repair_plans (id),
    generation INTEGER NOT NULL,
    name TEXT NOT NULL,
    kept_name TEXT NOT NULL,
    reason TEXT NOT NULL,
    sha256 TEXT,
    size INTEGER NOT NULL,
    mtime_ns TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'kept' CHECK (state IN ('kept', 'deleting', 'deleted', 'gone')),
    deletion_id INTEGER REFERENCES source_kept_deletions (id),
    kept_at TEXT NOT NULL,
    deleted_at TEXT,
    error TEXT,
    UNIQUE (source_set_id, generation, kept_name)
);

-- A durable intent: one step of an approved repair, committed with the approval before anything changes and advanced
-- before each change, so that recovery rolls it forward from where it stopped. detail is the RepairStep JSON.
CREATE TABLE repair_steps (
    id INTEGER PRIMARY KEY,
    plan_id INTEGER NOT NULL REFERENCES repair_plans (id),
    seq INTEGER NOT NULL,
    item TEXT NOT NULL,
    detail TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'staging', 'staged', 'moving', 'done', 'failed', 'skipped')),
    staged_name TEXT,
    staged_dev INTEGER,
    staged_ino INTEGER,
    sha256 TEXT,
    kept_id INTEGER REFERENCES source_kept (id),
    move_id INTEGER REFERENCES ingest_moves (id),
    error TEXT,
    note TEXT,
    finished_at TEXT,
    UNIQUE (plan_id, seq)
);
CREATE INDEX repair_steps_unfinished ON repair_steps (plan_id) WHERE state NOT IN ('done', 'failed', 'skipped');

-- Deleting kept files of a Source Set, as KeptDeletion does for a Managed Folder: SourceKeptDeletionContent JSON
-- and its digest; outcomes is a JSON array of KeptDeletionOutcome.
CREATE TABLE source_kept_deletions (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approved_digest TEXT,
    approved_by TEXT,
    approved_at TEXT,
    finished_at TEXT,
    outcomes TEXT,
    recovery_error TEXT
);
