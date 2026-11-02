-- Storage Policy (#46): how Source Sets and Play Libraries store each Game, as TorrentZip or loose, and whether a
-- conversion deletes each original once its converted file is verified.

-- One row per scope and system: system '' is the scope's default, which always sets both; a system's row overrides
-- what it sets, and follows the default where it is NULL. Systems are DATs, as `<provider>/<name>`.
CREATE TABLE storage_policy (
    scope TEXT NOT NULL CHECK (scope IN ('source_sets', 'play_libraries')),
    system TEXT NOT NULL,
    format TEXT CHECK (format IN ('torrent_zip', 'loose')),
    delete_originals INTEGER CHECK (delete_originals IN (0, 1)),
    updated_by TEXT,
    updated_at TEXT,
    PRIMARY KEY (scope, system),
    CHECK (system <> '' OR (format IS NOT NULL AND delete_originals IS NOT NULL))
);
INSERT INTO storage_policy (scope, system, format, delete_originals) VALUES
    ('source_sets', '', 'torrent_zip', 0),
    ('play_libraries', '', 'torrent_zip', 0);

-- A kept file's ctime read, just after the rename that kept it, through the descriptor held across that rename: with
-- the SHA-256 its conversion read it as, the only stamp that may authorize deleting a converted original.
ALTER TABLE source_kept ADD COLUMN kept_ctime_ns TEXT;
ALTER TABLE kept_files ADD COLUMN kept_ctime_ns TEXT;
-- The SHA-256 of the one file a conversion's Build step read, hashed as it was read while its stamp held still.
ALTER TABLE repair_steps ADD COLUMN source_sha256 TEXT;
-- A move that writes one ROM as a loose file, extracted from its ZIP, rather than a TorrentZip.
ALTER TABLE ingest_moves ADD COLUMN loose INTEGER NOT NULL DEFAULT 0;

-- A file built loose, its one ROM under its DAT name, rather than as a TorrentZip.
ALTER TABLE built_files ADD COLUMN loose INTEGER NOT NULL DEFAULT 0;
-- The ctime of a kept file, read through the descriptor held across its rename into the private folder it is
-- deleted from, as a conversion deletes it: what it must still have there to be deleted.
ALTER TABLE kept_files ADD COLUMN slot_ctime_ns TEXT;
-- A deletion of a conversion's original, approved with the plan that converted it: its file is deleted only by its
-- trusted stamp, in recovery too.
ALTER TABLE kept_deletions ADD COLUMN converted INTEGER NOT NULL DEFAULT 0;
