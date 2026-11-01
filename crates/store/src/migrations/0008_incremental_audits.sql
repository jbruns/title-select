-- Incremental Audits: Payload Evidence reused while a file is unchanged, Claim Evidence from a quick inventory, and
-- Audit Jobs that resume.

-- The last read of every payload of a file in a Source Set, by its path relative to the Source Set, kept for later
-- Audits to reuse while the file's dev, ino, size, mtime_ns and ctime_ns are all as they were when it was opened for
-- that read. This change test is a heuristic, not a reread. members is a JSON array of [member, checksums] pairs,
-- in archive order, member null for a loose file. Each file is recorded as soon as it is read, so an Audit cut off
-- part way resumes from what it finished. job_id is the Job that read it: a resumed full Audit reuses only what its
-- interrupted Job read. A file not found by an Audit that finishes is forgotten.
CREATE TABLE payload_reads (
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    path TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    read_at TEXT NOT NULL,
    job_id INTEGER REFERENCES jobs (id),
    members TEXT NOT NULL,
    PRIMARY KEY (source_set_id, path)
);

-- How an Audit got its checksums: 'quick', 'incremental' or 'full'. Every earlier Audit read every payload.
ALTER TABLE audits ADD COLUMN evidence TEXT NOT NULL DEFAULT 'full'
    CHECK (evidence IN ('quick', 'incremental', 'full'));

-- How an Audit Job's Audit gets its checksums; null for other kinds of Job.
ALTER TABLE jobs ADD COLUMN evidence TEXT CHECK (evidence IN ('quick', 'incremental', 'full'));
UPDATE jobs SET evidence = 'full' WHERE kind = 'audit';

-- For Payload Evidence, when the payload was last read, and whether the Audit reused that read under the change test
-- rather than reading it. Every earlier Audit read its payloads while it ran.
ALTER TABLE audit_entries ADD COLUMN read_at TEXT;
ALTER TABLE audit_entries ADD COLUMN reused INTEGER NOT NULL DEFAULT 0;
UPDATE audit_entries SET read_at = (SELECT started_at FROM audits a WHERE a.id = audit_entries.audit_id)
WHERE evidence_level = 'payload';
