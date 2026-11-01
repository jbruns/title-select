-- Ingest Folders (#16): Roots of kind `ingest`, registered by writing their marker. generation counts the plans
-- applied to the folder, each of which keeps its items under .title-select-retained/<generation>/.
CREATE TABLE ingest_folders (
    id INTEGER PRIMARY KEY,
    root TEXT NOT NULL UNIQUE,
    generation INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    last_scan_at TEXT
);

-- The directories Title Select created in an Ingest Folder, as folder_dirs, folder_temps and folder_temp_names are
-- for a Managed Folder: the retention area, each generation's folder, and the private .deleting folder.
CREATE TABLE ingest_dirs (
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE ingest_temps (
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE ingest_temp_names (
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    name TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- An entry found in an Ingest Folder. dev, ino, size, mtime_ns and ctime_ns are as last seen; stable_since is when
-- they were first seen as they are. hashed_* are the stat the assessment's hashes were read under, reused while the
-- entry is still exactly that. Only one item that is not gone or moved holds a name.
CREATE TABLE ingest_items (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    name TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('arriving', 'assessed', 'unreadable', 'failed', 'stale', 'gone',
                                         'already_held', 'unsupported', 'dismissed', 'moved')),
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns TEXT NOT NULL,
    ctime_ns TEXT NOT NULL,
    stable_since TEXT NOT NULL,
    hashed_dev INTEGER,
    hashed_ino INTEGER,
    hashed_size INTEGER,
    hashed_mtime_ns TEXT,
    hashed_ctime_ns TEXT,
    probe TEXT,
    assessment TEXT,
    reason TEXT,
    first_seen_at TEXT NOT NULL,
    assessed_at TEXT,
    changed_at TEXT NOT NULL
);
CREATE UNIQUE INDEX ingest_items_by_name ON ingest_items (folder_id, name) WHERE state NOT IN ('gone', 'moved');

-- A plan for items of an Ingest Folder: IngestPlanContent JSON and the SHA-256 of that JSON, which an approval binds
-- to. It is carried out when approved; outcomes is a JSON array of IngestOutcome.
CREATE TABLE ingest_plans (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
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
CREATE INDEX ingest_plans_by_folder ON ingest_plans (folder_id, id);

-- A file kept aside in an Ingest Folder's .title-select-retained/<generation>/, by the plan that moved it there, if
-- a plan did.
CREATE TABLE ingest_kept (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    plan_id INTEGER REFERENCES ingest_plans (id),
    item_id INTEGER REFERENCES ingest_items (id),
    generation INTEGER NOT NULL,
    name TEXT NOT NULL,
    kept_name TEXT NOT NULL,
    reason TEXT NOT NULL,
    sha256 TEXT,
    size INTEGER NOT NULL,
    mtime_ns TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    kept_at TEXT NOT NULL,
    UNIQUE (folder_id, generation, kept_name)
);

-- A durable intent to move one entry out of an Ingest Folder, committed before anything is moved and advanced
-- before each step, so that a stop rolls forward from where it was. kind is how it moves: one rename that never
-- replaces anything, a verified copy whose original is then deleted, or a verified TorrentZip repack whose original
-- is then kept. dest_kind 'kept' is the folder's own kept files, under generation; 'path' is the folder rel in the
-- writable Root dest_root. staged_* is the file a copy or repack writes before it is renamed to dest_name.
CREATE TABLE ingest_moves (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    plan_id INTEGER REFERENCES ingest_plans (id),
    item_id INTEGER REFERENCES ingest_items (id),
    kind TEXT NOT NULL CHECK (kind IN ('rename', 'copy', 'repack')),
    state TEXT NOT NULL CHECK (state IN ('pending', 'moving', 'restoring', 'staging', 'published', 'disposing',
                                         'keeping', 'done', 'failed')),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns TEXT NOT NULL,
    ctime_ns TEXT NOT NULL,
    sha256 TEXT,
    dest_kind TEXT NOT NULL CHECK (dest_kind IN ('kept', 'path')),
    dest_root TEXT,
    dest_rel TEXT,
    generation INTEGER,
    dest_name TEXT NOT NULL,
    reason TEXT,
    roms TEXT,
    staged_name TEXT,
    staged_dev INTEGER,
    staged_ino INTEGER,
    dest_sha256 TEXT,
    -- An entry found where the planned one should have been, and the kept files name it is set aside under.
    found_dev INTEGER,
    found_ino INTEGER,
    aside_name TEXT,
    kept_id INTEGER REFERENCES ingest_kept (id),
    error TEXT,
    -- What a move that ended did besides what was planned, such as keeping an original that changed.
    note TEXT,
    created_at TEXT NOT NULL,
    finished_at TEXT
);
CREATE INDEX ingest_moves_unfinished ON ingest_moves (state) WHERE state NOT IN ('done', 'failed');

-- A Job may now scan an Ingest Folder, which has no Source Set or DAT Revision. SQLite cannot change a column's
-- constraints, so jobs is rebuilt again; the migration runs with foreign keys off and checks them before it commits.
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
    kind TEXT NOT NULL DEFAULT 'audit' CHECK (kind IN ('audit', 'selection', 'build', 'check', 'ingest_scan')),
    profile_version_id INTEGER REFERENCES profile_versions (id),
    selection_id INTEGER REFERENCES selections (id),
    managed_folder_id INTEGER REFERENCES managed_folders (id),
    plan_id INTEGER REFERENCES plans (id),
    evidence TEXT CHECK (evidence IN ('quick', 'incremental', 'full')),
    ingest_folder_id INTEGER REFERENCES ingest_folders (id),
    CHECK ((kind = 'ingest_scan') = (source_set_id IS NULL AND dat_revision_id IS NULL)),
    CHECK ((kind = 'ingest_scan') = (ingest_folder_id IS NOT NULL))
);
INSERT INTO jobs_new (id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error,
                      readers, hashers, created_at, started_at, finished_at, version, kind, profile_version_id,
                      selection_id, managed_folder_id, plan_id, evidence)
SELECT id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error, readers, hashers,
       created_at, started_at, finished_at, version, kind, profile_version_id, selection_id, managed_folder_id, plan_id,
       evidence
FROM jobs;
DROP TABLE jobs;
ALTER TABLE jobs_new RENAME TO jobs;
CREATE INDEX jobs_by_state ON jobs (state, id);
CREATE UNIQUE INDEX jobs_one_active_per_source_set ON jobs (source_set_id, kind)
WHERE state IN ('queued', 'running') AND kind IN ('audit', 'selection');
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

-- An Audit a DAT import owes each Source Set bound to the imported DAT, recorded before the import so that one
-- missed while another Audit of the set ran, or by a stop, is queued later.
CREATE TABLE ingest_reaudits (
    id INTEGER PRIMARY KEY,
    plan_id INTEGER NOT NULL REFERENCES ingest_plans (id),
    item_id INTEGER NOT NULL REFERENCES ingest_items (id),
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    sha256 TEXT NOT NULL,
    actor TEXT NOT NULL,
    readers INTEGER NOT NULL,
    hashers INTEGER NOT NULL,
    job_id INTEGER,
    created_at TEXT NOT NULL,
    fulfilled_at TEXT,
    UNIQUE (plan_id, item_id, source_set_id)
);
CREATE INDEX ingest_reaudits_owed ON ingest_reaudits (id) WHERE fulfilled_at IS NULL;
