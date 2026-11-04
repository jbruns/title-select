-- Lifecycle: a registration can be Unregistered, which keeps its row, and its history, read-only. Its name and
-- folder are then free for a new registration, so each uniqueness holds among registrations still registered only.
-- Forgetting an Unregistered registration later deletes its row and history.

CREATE TABLE source_sets_new (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    dat_id INTEGER NOT NULL REFERENCES dats (id),
    added_at TEXT NOT NULL,
    follow INTEGER NOT NULL DEFAULT 0,
    unregistered_at TEXT
);
INSERT INTO source_sets_new (id, name, root, path, dat_id, added_at, follow)
    SELECT id, name, root, path, dat_id, added_at, follow FROM source_sets;
DROP TABLE source_sets;
ALTER TABLE source_sets_new RENAME TO source_sets;
CREATE UNIQUE INDEX source_sets_name ON source_sets (name) WHERE unregistered_at IS NULL;
CREATE UNIQUE INDEX source_sets_folder ON source_sets (root, path) WHERE unregistered_at IS NULL;

CREATE TABLE library_folders_new (
    id INTEGER PRIMARY KEY,
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    profile_id INTEGER NOT NULL REFERENCES profiles (id),
    generation INTEGER NOT NULL DEFAULT 0,
    current_build_id INTEGER REFERENCES builds (id),
    created_at TEXT NOT NULL,
    unregistered_at TEXT
);
INSERT INTO library_folders_new (id, root, path, source_set_id, profile_id, generation, current_build_id, created_at)
    SELECT id, root, path, source_set_id, profile_id, generation, current_build_id, created_at FROM library_folders;
DROP TABLE library_folders;
ALTER TABLE library_folders_new RENAME TO library_folders;
CREATE UNIQUE INDEX library_folders_folder ON library_folders (root, path) WHERE unregistered_at IS NULL;

-- setting_aside_at: when you approved setting the Additions Set aside; it is Unregistered once every addition is
-- retired.
CREATE TABLE additions_sets_new (
    id INTEGER PRIMARY KEY,
    dat_id INTEGER NOT NULL REFERENCES dats (id),
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    dev INTEGER,
    ino INTEGER,
    temp_name TEXT,
    temp_dev INTEGER,
    temp_ino INTEGER,
    marker_at TEXT,
    created_at TEXT NOT NULL,
    setting_aside_at TEXT,
    unregistered_at TEXT
);
INSERT INTO additions_sets_new (id, dat_id, root, path, dev, ino, temp_name, temp_dev, temp_ino, marker_at,
                                created_at)
    SELECT id, dat_id, root, path, dev, ino, temp_name, temp_dev, temp_ino, marker_at, created_at
    FROM additions_sets;
DROP TABLE additions_sets;
ALTER TABLE additions_sets_new RENAME TO additions_sets;
CREATE UNIQUE INDEX additions_sets_dat ON additions_sets (dat_id) WHERE unregistered_at IS NULL;
CREATE UNIQUE INDEX additions_sets_folder ON additions_sets (root, path) WHERE unregistered_at IS NULL;

CREATE TABLE ingest_folders_new (
    id INTEGER PRIMARY KEY,
    root TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    last_scan_at TEXT,
    unregistered_at TEXT
);
INSERT INTO ingest_folders_new (id, root, generation, created_at, last_scan_at)
    SELECT id, root, generation, created_at, last_scan_at FROM ingest_folders;
DROP TABLE ingest_folders;
ALTER TABLE ingest_folders_new RENAME TO ingest_folders;
CREATE UNIQUE INDEX ingest_folders_root ON ingest_folders (root) WHERE unregistered_at IS NULL;

-- A Retired Selection Profile is hidden from new choices; its versions stay for the Selections that used them.
ALTER TABLE profiles ADD COLUMN retired_at TEXT;

-- A move of a Library Folder to another path in its Root: planned, approved as reviewed with digest, then carried
-- out by one rename that never replaces anything. moving_at is committed before the rename, so that a stop between
-- the rename and the record is settled, when the service next starts, by where the folder (dev, ino) is found.
CREATE TABLE library_folder_moves (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    root TEXT NOT NULL,
    from_path TEXT NOT NULL,
    to_path TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approved_by TEXT,
    approved_at TEXT,
    finished_at TEXT,
    outcome TEXT
);
