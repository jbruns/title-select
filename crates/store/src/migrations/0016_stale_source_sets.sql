-- Stale Source Sets (#79): a routine stat walk of each Source Set, and Follow.

-- Whether the Source Set Follows its DAT: a DAT Revision of it newer than its latest Audit's then makes it Stale.
-- Following never starts an Audit.
ALTER TABLE source_sets ADD COLUMN follow INTEGER NOT NULL DEFAULT 0;

-- What a stat walk found, compared with the Source Set's latest Audit when it ran, audit_id: how many files' stamps
-- fail the change test, how many files that Audit did not see, and how many it saw that are gone. A walk that finds
-- what the previous one of the Source Set found against the same Audit only moves checked_at and job_id on. A walk
-- that found nothing replaces the earlier walks of its Source Set that found nothing. The Source Set is Stale while
-- its latest walk is against its latest Audit and found something.
CREATE TABLE stat_walks (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    job_id INTEGER REFERENCES jobs (id),
    found_at TEXT NOT NULL,
    checked_at TEXT NOT NULL,
    changed INTEGER NOT NULL,
    added INTEGER NOT NULL,
    removed INTEGER NOT NULL
);
CREATE INDEX stat_walks_by_source_set ON stat_walks (source_set_id, id);

-- The stamp of each ZIP archive a quick inventory took claims from, recorded as it opened it, so that a stat walk can
-- tell whether it changed since. Kept apart from audit_stamps, which a repair acts on.
CREATE TABLE audit_claim_stamps (
    audit_id INTEGER NOT NULL REFERENCES audits (id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    PRIMARY KEY (audit_id, path)
);

-- A stat walk is a Job of kind 'stat_walk' of a Source Set, with no DAT Revision. jobs is rebuilt again to allow the
-- new kind; the migration runs with foreign keys off and checks them before it commits.
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
        CHECK (kind IN ('audit', 'selection', 'build', 'check', 'ingest_scan', 'repair', 'additions', 'stat_walk')),
    profile_version_id INTEGER REFERENCES profile_versions (id),
    selection_id INTEGER REFERENCES selections (id),
    library_folder_id INTEGER REFERENCES library_folders (id),
    plan_id INTEGER REFERENCES plans (id),
    evidence TEXT CHECK (evidence IN ('quick', 'incremental', 'full')),
    ingest_folder_id INTEGER REFERENCES ingest_folders (id),
    repair_plan_id INTEGER REFERENCES repair_plans (id),
    cancel_requested_at TEXT,
    additions_set_id INTEGER REFERENCES additions_sets (id),
    CHECK ((kind IN ('ingest_scan', 'additions')) = (source_set_id IS NULL AND dat_revision_id IS NULL)),
    CHECK ((kind = 'ingest_scan') = (ingest_folder_id IS NOT NULL)),
    CHECK ((kind = 'repair') = (repair_plan_id IS NOT NULL)),
    CHECK ((kind = 'additions') = (additions_set_id IS NOT NULL)),
    CHECK (kind <> 'stat_walk' OR dat_revision_id IS NULL)
);
INSERT INTO jobs_new (id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error,
                      readers, hashers, created_at, started_at, finished_at, version, kind, profile_version_id,
                      selection_id, library_folder_id, plan_id, evidence, ingest_folder_id, repair_plan_id,
                      cancel_requested_at, additions_set_id)
SELECT id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error, readers, hashers,
       created_at, started_at, finished_at, version, kind, profile_version_id, selection_id, library_folder_id, plan_id,
       evidence, ingest_folder_id, repair_plan_id, cancel_requested_at, additions_set_id
FROM jobs;
DROP TABLE jobs;
ALTER TABLE jobs_new RENAME TO jobs;
CREATE INDEX jobs_by_state ON jobs (state, id);
CREATE INDEX jobs_by_repair_plan ON jobs (repair_plan_id) WHERE repair_plan_id IS NOT NULL;
CREATE UNIQUE INDEX jobs_one_active_per_source_set ON jobs (source_set_id, kind)
WHERE state IN ('queued', 'running') AND kind IN ('audit', 'selection', 'repair', 'stat_walk');
CREATE UNIQUE INDEX jobs_one_active_per_folder ON jobs (library_folder_id)
WHERE state IN ('queued', 'running') AND kind IN ('build', 'check');
CREATE UNIQUE INDEX jobs_one_active_per_ingest_folder ON jobs (ingest_folder_id)
WHERE state IN ('queued', 'running') AND kind = 'ingest_scan';
CREATE UNIQUE INDEX jobs_one_queued_per_additions_set ON jobs (additions_set_id)
WHERE state = 'queued' AND kind = 'additions' AND cancel_requested_at IS NULL;
CREATE TRIGGER jobs_kind_profile BEFORE INSERT ON jobs
WHEN (NEW.kind = 'selection') <> (NEW.profile_version_id IS NOT NULL)
  OR (NEW.kind = 'build') <> (NEW.plan_id IS NOT NULL)
  OR (NEW.kind IN ('build', 'check')) <> (NEW.library_folder_id IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'a selection job runs a profile version, a build job an approved plan, a check job a library folder, an audit job none');
END;
