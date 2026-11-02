-- Operator Additions (#19): files no DAT knows that the Operator chose to keep, each identified by its own hashes, in
-- an Additions Set per system (DAT) at <path> in the Root named 'additions'.

-- An Additions Set: its folder, created by the first addition of its system under the recorded temporary name
-- temp_name, checked to be the private, empty directory made (temp_dev, temp_ino), given its marker, and renamed to
-- path without replacing anything; dev and ino are recorded once it is in place, with marker_at.
CREATE TABLE additions_sets (
    id INTEGER PRIMARY KEY,
    dat_id INTEGER NOT NULL UNIQUE REFERENCES dats (id),
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    dev INTEGER,
    ino INTEGER,
    temp_name TEXT,
    temp_dev INTEGER,
    temp_ino INTEGER,
    marker_at TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (root, path)
);

-- A temporary folder an attempt to create an Additions Set's folder left behind, by name and, when it was recorded,
-- identity: recorded before another attempt replaces temp_name, so that it is never forgotten. It is left where it is,
-- for you to remove, and never deleted by Title Select.
CREATE TABLE additions_set_leftovers (
    id INTEGER PRIMARY KEY,
    set_id INTEGER NOT NULL REFERENCES additions_sets (id),
    name TEXT NOT NULL,
    dev INTEGER,
    ino INTEGER,
    found_at TEXT NOT NULL,
    UNIQUE (set_id, name)
);

-- The directories Title Select created in an Additions Set's folder to keep retired additions' files, and those being
-- created, as folder_dirs, folder_temps and folder_temp_names record them for a Managed Folder.
CREATE TABLE additions_dirs (
    folder_id INTEGER NOT NULL REFERENCES additions_sets (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE additions_temps (
    folder_id INTEGER NOT NULL REFERENCES additions_sets (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE additions_temp_names (
    folder_id INTEGER NOT NULL REFERENCES additions_sets (id),
    name TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- An Operator Addition. rom is the ExpectedRom JSON of its one ROM, with its own hashes, read through a descriptor
-- held on the file while it stayed still, and recorded when it was approved: by a scan for an ingest item, by the
-- repair plan for a Source Set Entry and by the check for drift. Its file is file_name in the set, written as
-- TorrentZip or loose as loose says, and is published only once it holds exactly that ROM.
--
-- How it gets there is a durable intent, recorded before anything changes:
-- - from an Ingest Folder, by the ingest move move_id, which writes file_name itself;
-- - from a Source Set, once the Keep step of item repair_item of repair repair_plan_id has kept it aside;
-- - from Drift, once the cleanup plan cleanup_plan_id has kept the entry origin aside;
-- the latter two are then published from that kept file, which is never deleted: staged_name is recorded before the
-- staged file is created, staged_dev and staged_ino once it is, and the staged file is renamed to file_name without
-- replacing anything.
--
-- dev, ino, size, mtime_ns, ctime_ns and sha256 are its published file as Title Select wrote and read it back through
-- the descriptor it held: what a build reads, and what a check compares the folder with.
CREATE TABLE additions (
    id INTEGER PRIMARY KEY,
    set_id INTEGER NOT NULL REFERENCES additions_sets (id),
    name TEXT NOT NULL,
    file_name TEXT NOT NULL,
    rom TEXT NOT NULL,
    parent_game TEXT,
    source TEXT NOT NULL CHECK (source IN ('ingest', 'drift', 'source_set')),
    origin TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK (state IN ('pending', 'staging', 'active', 'retiring', 'retired', 'restoring', 'failed')),
    error TEXT,
    move_id INTEGER REFERENCES ingest_moves (id),
    repair_plan_id INTEGER REFERENCES repair_plans (id),
    repair_item TEXT,
    cleanup_plan_id INTEGER REFERENCES plans (id),
    staged_name TEXT,
    staged_dev INTEGER,
    staged_ino INTEGER,
    dev INTEGER,
    ino INTEGER,
    size INTEGER,
    mtime_ns TEXT,
    ctime_ns TEXT,
    sha256 TEXT,
    approved_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    published_at TEXT,
    retired_at TEXT,
    -- How many times you asked to retire or restore it: one asked after its Additions Job visited it is handed on.
    requests INTEGER NOT NULL DEFAULT 0,
    -- Whether file_name is a loose file, its ROM itself, rather than a TorrentZip: what the storage policy said for
    -- its system's source sets when it was approved.
    loose INTEGER NOT NULL DEFAULT 0,
    CHECK ((source = 'ingest') = (move_id IS NOT NULL)),
    CHECK ((source = 'source_set') = (repair_plan_id IS NOT NULL AND repair_item IS NOT NULL)),
    CHECK ((source = 'drift') = (cleanup_plan_id IS NOT NULL)),
    CHECK (state NOT IN ('active', 'retiring', 'restoring') OR (dev IS NOT NULL AND sha256 IS NOT NULL))
);
CREATE UNIQUE INDEX additions_file_in_use ON additions (set_id, file_name)
WHERE state IN ('pending', 'staging', 'active', 'retiring', 'restoring');
CREATE UNIQUE INDEX additions_name_in_use ON additions (set_id, name)
WHERE state IN ('pending', 'staging', 'active', 'retiring', 'restoring');
CREATE INDEX additions_unfinished ON additions (state)
WHERE state IN ('pending', 'staging', 'retiring', 'restoring');

-- Deleting kept files of an Additions Set, as source_kept_deletions records it for a Source Set.
CREATE TABLE additions_kept_deletions (
    id INTEGER PRIMARY KEY,
    additions_set_id INTEGER NOT NULL REFERENCES additions_sets (id),
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

-- A retired addition's file, kept in its Additions Set at `.title-select-retained/<generation>/<kept_name>`, where
-- generation is the addition's id; name is the file name it was published under. A retirement is recorded here as
-- 'keeping', with the published file's identity, size, mtime and SHA-256 that Title Select wrote and read back through
-- the descriptor it held, before the file is moved; it is 'kept' once it is found there, and 'restored' once a
-- restoration moved it back. It is deleted only as source_kept is, once you approve it, and only while it is still the
-- file recorded, read through a descriptor while it holds still; a stamp read from a path never authorizes it.
CREATE TABLE additions_kept (
    id INTEGER PRIMARY KEY,
    additions_set_id INTEGER NOT NULL REFERENCES additions_sets (id),
    addition_id INTEGER NOT NULL REFERENCES additions (id),
    generation INTEGER NOT NULL,
    name TEXT NOT NULL,
    kept_name TEXT NOT NULL,
    reason TEXT NOT NULL,
    sha256 TEXT,
    size INTEGER NOT NULL,
    mtime_ns TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'keeping'
        CHECK (state IN ('keeping', 'kept', 'deleting', 'deleted', 'gone', 'restored')),
    deletion_id INTEGER REFERENCES additions_kept_deletions (id),
    kept_at TEXT NOT NULL,
    deleted_at TEXT,
    error TEXT,
    slot_ctime_ns TEXT
);
CREATE UNIQUE INDEX additions_kept_in_place ON additions_kept (additions_set_id, generation, kept_name)
WHERE state IN ('keeping', 'kept', 'deleting');

-- An Additions Job publishes, retires and restores the additions of one Additions Set, additions_set_id: whatever
-- approves one records its intent and queues the Job, which a stop leaves queued again. Only one you have not
-- cancelled waits per set at a time. jobs is rebuilt again to allow the new kind; the migration runs with foreign keys off and checks them before
-- it commits.
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
        CHECK (kind IN ('audit', 'selection', 'build', 'check', 'ingest_scan', 'repair', 'additions')),
    profile_version_id INTEGER REFERENCES profile_versions (id),
    selection_id INTEGER REFERENCES selections (id),
    managed_folder_id INTEGER REFERENCES managed_folders (id),
    plan_id INTEGER REFERENCES plans (id),
    evidence TEXT CHECK (evidence IN ('quick', 'incremental', 'full')),
    ingest_folder_id INTEGER REFERENCES ingest_folders (id),
    repair_plan_id INTEGER REFERENCES repair_plans (id),
    cancel_requested_at TEXT,
    additions_set_id INTEGER REFERENCES additions_sets (id),
    CHECK ((kind IN ('ingest_scan', 'additions')) = (source_set_id IS NULL AND dat_revision_id IS NULL)),
    CHECK ((kind = 'ingest_scan') = (ingest_folder_id IS NOT NULL)),
    CHECK ((kind = 'repair') = (repair_plan_id IS NOT NULL)),
    CHECK ((kind = 'additions') = (additions_set_id IS NOT NULL))
);
INSERT INTO jobs_new (id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error,
                      readers, hashers, created_at, started_at, finished_at, version, kind, profile_version_id,
                      selection_id, managed_folder_id, plan_id, evidence, ingest_folder_id, repair_plan_id,
                      cancel_requested_at)
SELECT id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error, readers, hashers,
       created_at, started_at, finished_at, version, kind, profile_version_id, selection_id, managed_folder_id, plan_id,
       evidence, ingest_folder_id, repair_plan_id, cancel_requested_at
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
-- One queued Additions Job per Additions Set takes new members; one you cancelled takes none, and does not count.
CREATE UNIQUE INDEX jobs_one_queued_per_additions_set ON jobs (additions_set_id)
WHERE state = 'queued' AND kind = 'additions' AND cancel_requested_at IS NULL;
CREATE TRIGGER jobs_kind_profile BEFORE INSERT ON jobs
WHEN (NEW.kind = 'selection') <> (NEW.profile_version_id IS NOT NULL)
  OR (NEW.kind = 'build') <> (NEW.plan_id IS NOT NULL)
  OR (NEW.kind IN ('build', 'check')) <> (NEW.managed_folder_id IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'a selection job runs a profile version, a build job an approved plan, a check job a managed folder, an audit job none');
END;

-- The Operator Addition a built file was built from, when it was.
ALTER TABLE built_files ADD COLUMN addition_id INTEGER REFERENCES additions (id);

-- The additions each Additions Job settles: those to settle when it was queued, and those that joined it before it
-- started. An addition its Job left to settle joins a later one.
CREATE TABLE additions_job_members (
    job_id INTEGER NOT NULL REFERENCES jobs (id),
    addition_id INTEGER NOT NULL REFERENCES additions (id),
    PRIMARY KEY (job_id, addition_id)
);
CREATE INDEX additions_job_members_by_addition ON additions_job_members (addition_id, job_id);
