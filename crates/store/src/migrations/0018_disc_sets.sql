-- Disc Source Sets (#97): the loose folder format, the SHA-256 of each file a Build step read, and Redump cue packs.

-- The storage policy allows 'loose_folder'. SQLite can't alter a CHECK, so the table is rebuilt.
CREATE TABLE storage_policy_new (
    scope TEXT NOT NULL CHECK (scope IN ('source_sets', 'play_libraries')),
    system TEXT NOT NULL,
    format TEXT CHECK (format IN ('torrent_zip', 'loose', 'loose_folder')),
    delete_originals INTEGER CHECK (delete_originals IN (0, 1)),
    updated_by TEXT,
    updated_at TEXT,
    PRIMARY KEY (scope, system),
    CHECK (system <> '' OR (format IS NOT NULL AND delete_originals IS NOT NULL))
);
INSERT INTO storage_policy_new SELECT scope, system, format, delete_originals, updated_by, updated_at
    FROM storage_policy;
DROP TABLE storage_policy;
ALTER TABLE storage_policy_new RENAME TO storage_policy;

-- A JSON object of each file a Build step read, by its path relative to the Source Set's folder, to the SHA-256 it
-- read it as while its stamp held still: a conversion of a loose folder into one TorrentZip reads several originals,
-- and each is deleted only by what was read of it. source_sha256 still records a Build step that read one file.
ALTER TABLE repair_steps ADD COLUMN source_reads TEXT;

-- A Redump cue pack: a ZIP of `.cue` sheets for one DAT, kept unmodified under `cue_packs/<sha256>` in the store,
-- with where it came from. A repair fills or replaces a `.cue` ROM from it, matched by the DAT's checksums.
-- origin is 'import' for one you imported, 'fetch' for one fetched (#96).
CREATE TABLE cue_packs (
    id INTEGER PRIMARY KEY,
    sha256 TEXT NOT NULL UNIQUE,
    dat TEXT NOT NULL,
    file_name TEXT NOT NULL,
    source_url TEXT,
    origin TEXT NOT NULL CHECK (origin IN ('import', 'fetch')),
    cues INTEGER NOT NULL,
    imported_at TEXT NOT NULL
);
CREATE INDEX cue_packs_by_dat ON cue_packs (dat);

-- An ingest move that leaves its original where it is once its copy is published: one of several extracting the ROMs
-- of a ZIP into a loose folder, the last of which disposes of the ZIP.
ALTER TABLE ingest_moves ADD COLUMN leave_original INTEGER NOT NULL DEFAULT 0;
