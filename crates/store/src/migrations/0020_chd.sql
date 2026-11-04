-- CHD builds (#99): a disc system's Play Libraries may store each disc as a CHD that chdman builds.

-- The storage policy allows 'chd', for play libraries only: a Source Set holds the DAT's exact bytes (ADR 0013).
-- SQLite can't alter a CHECK, so the table is rebuilt.
CREATE TABLE storage_policy_new (
    scope TEXT NOT NULL CHECK (scope IN ('source_sets', 'play_libraries')),
    system TEXT NOT NULL,
    format TEXT CHECK (format IN ('torrent_zip', 'loose', 'loose_folder', 'chd')),
    delete_originals INTEGER CHECK (delete_originals IN (0, 1)),
    updated_by TEXT,
    updated_at TEXT,
    PRIMARY KEY (scope, system),
    CHECK (system <> '' OR (format IS NOT NULL AND delete_originals IS NOT NULL)),
    CHECK (format IS NOT 'chd' OR (scope = 'play_libraries' AND system <> ''))
);
INSERT INTO storage_policy_new SELECT scope, system, format, delete_originals, updated_by, updated_at
    FROM storage_policy;
DROP TABLE storage_policy;
ALTER TABLE storage_policy_new RENAME TO storage_policy;

-- A built file that is a CHD, and the raw data SHA-1 its v5 header recorded when its extraction was checked against
-- the DAT: a later check reads the header alone and compares it, never decompressing. The raw data SHA-1 covers the
-- disc's data only, not chdman's compression or metadata, so a newer chdman never changes it.
ALTER TABLE built_files ADD COLUMN chd INTEGER NOT NULL DEFAULT 0;
ALTER TABLE built_files ADD COLUMN chd_sha1 TEXT;

-- The raw data SHA-1 of the CHD an item verified and is putting in place, recorded with its publish intent so that
-- recovery records it built as the Build would have.
ALTER TABLE build_items ADD COLUMN chd_sha1 TEXT;
