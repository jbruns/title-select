-- Source Set repair (#17): repair plans and their steps, and the kept files repairs make in Source Sets.

-- A Source Set a repair may write to. generation counts approved repairs; each keeps files under its own
-- `.title-select-retained/<generation>/`. marker_at is when its marker was written, by the first approved repair.
CREATE TABLE source_folders (
    source_set_id INTEGER PRIMARY KEY REFERENCES source_sets (id),
    generation INTEGER NOT NULL DEFAULT 0,
    marker_at TEXT,
    -- The first marker's temporary file: marker_temp_pending is committed before it is created, and its identity
    -- once it is, so that recovery writes and publishes that file, and only it.
    marker_temp_pending INTEGER NOT NULL DEFAULT 0,
    marker_temp_dev INTEGER,
    marker_temp_ino INTEGER
);
-- The stamp each file of a Source Set had when its last Audit could not read one of its payloads, as payload_reads
-- records one it read in full, so that a repair tells whether it changed since.
CREATE TABLE unreadable_reads (
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    path TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    PRIMARY KEY (source_set_id, path)
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

-- An approved repair runs as a Repair Job of its Source Set, which carries the repair plan's id. cancel_requested_at
-- is when you asked a Job to stop at its next safe point. jobs is rebuilt again to allow the new kind; the migration
-- runs with foreign keys off and checks them before it commits.
CREATE TABLE jobs_new (
    id INTEGER PRIMARY KEY,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'interrupted')),
    source_set_id INTEGER REFERENCES source_sets (id),
    dat_revision_id INTEGER REFERENCES dat_revisions (id),
    actor TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 1,
    requeued_from INTEGER REFERENCES jobs (id),
    audit_id INTEGER REFERENCES audits (id),
    error TEXT,
    readers INTEGER NOT NULL,
    hashers INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    version INTEGER NOT NULL DEFAULT 1,
    kind TEXT NOT NULL DEFAULT 'audit'
        CHECK (kind IN ('audit', 'selection', 'build', 'check', 'ingest_scan', 'repair')),
    profile_version_id INTEGER REFERENCES profile_versions (id),
    selection_id INTEGER REFERENCES selections (id),
    managed_folder_id INTEGER REFERENCES managed_folders (id),
    plan_id INTEGER REFERENCES plans (id),
    evidence TEXT CHECK (evidence IN ('quick', 'incremental', 'full')),
    ingest_folder_id INTEGER REFERENCES ingest_folders (id),
    repair_plan_id INTEGER REFERENCES repair_plans (id),
    cancel_requested_at TEXT,
    CHECK ((kind = 'ingest_scan') = (source_set_id IS NULL AND dat_revision_id IS NULL)),
    CHECK ((kind = 'ingest_scan') = (ingest_folder_id IS NOT NULL)),
    CHECK ((kind = 'repair') = (repair_plan_id IS NOT NULL))
);
INSERT INTO jobs_new (id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error,
                      readers, hashers, created_at, started_at, finished_at, version, kind, profile_version_id,
                      selection_id, managed_folder_id, plan_id, evidence, ingest_folder_id)
SELECT id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error, readers, hashers,
       created_at, started_at, finished_at, version, kind, profile_version_id, selection_id, managed_folder_id, plan_id,
       evidence, ingest_folder_id
FROM jobs;
DROP TABLE jobs;
ALTER TABLE jobs_new RENAME TO jobs;
CREATE INDEX jobs_by_state ON jobs (state, id);
CREATE UNIQUE INDEX jobs_one_active_per_source_set ON jobs (source_set_id, kind)
WHERE state IN ('queued', 'running') AND kind IN ('audit', 'selection', 'repair');
CREATE UNIQUE INDEX jobs_one_active_per_folder ON jobs (managed_folder_id)
WHERE state IN ('queued', 'running') AND kind IN ('build', 'check');
CREATE UNIQUE INDEX jobs_one_active_per_ingest_folder ON jobs (ingest_folder_id)
WHERE state IN ('queued', 'running') AND kind = 'ingest_scan';
CREATE TRIGGER jobs_kind_profile BEFORE INSERT ON jobs
WHEN (NEW.kind = 'selection') <> (NEW.profile_version_id IS NOT NULL)
  OR (NEW.kind = 'build') <> (NEW.plan_id IS NOT NULL)
  OR (NEW.kind IN ('build', 'check')) <> (NEW.managed_folder_id IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'a selection job runs a profile version, a build job an approved plan, a check job a managed folder, an audit job none');
END;
