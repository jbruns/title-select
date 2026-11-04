-- Fetching (ADR 0012): every request to a DAT Provider, recorded before it is sent and finished with its outcome.
-- The latest request of each system and artifact holds the next back for a day, whatever its outcome.

-- Each DAT Revision's place among its DAT's revisions, by its header's version, then date: the highest is the newest,
-- whatever order they were imported in. Renumbered within the DAT on each import. Once this has run, the store
-- reads the date of each existing revision whose file it keeps and numbers every DAT's revisions afresh.
-- header_date: the header's date as written; empty if it gives none, or its file was gone when this ran.
ALTER TABLE dat_revisions ADD COLUMN header_order INTEGER NOT NULL DEFAULT 0;
ALTER TABLE dat_revisions ADD COLUMN header_date TEXT NOT NULL DEFAULT '';
UPDATE dat_revisions SET header_order = id;

-- A Redump SBI pack a fetch brought: a ZIP of subchannel files for one DAT, kept unmodified under
-- `sbi_packs/<sha256>`. Its file is deleted, and blob_dropped_at set, once no repair plan refers to it and the latest
-- successful fetch of its system's SBI pack brought another; the row stays.
CREATE TABLE sbi_packs (
    id INTEGER PRIMARY KEY,
    sha256 TEXT NOT NULL UNIQUE,
    dat TEXT NOT NULL,
    file_name TEXT NOT NULL,
    source_url TEXT,
    fetched_at TEXT NOT NULL,
    blob_dropped_at TEXT
);
CREATE INDEX sbi_packs_by_dat ON sbi_packs (dat);

-- system: Redump's system code, or for MAME `mame` (its release and ListXML) or a software list's name.
-- outcome: added, unchanged or failed; absent while the request runs, or if the Service stopped during it.
-- pack_sha256: the cue pack (in cue_packs) or SBI pack (in sbi_packs) the request brought.
-- release, asset_url: for MAME, the release found or fetched from, and its ListXML asset.
CREATE TABLE fetches (
    id INTEGER PRIMARY KEY,
    provider TEXT NOT NULL,
    system TEXT NOT NULL,
    artifact TEXT NOT NULL,
    trigger TEXT NOT NULL,
    actor TEXT NOT NULL,
    url TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    outcome TEXT,
    error TEXT,
    dat_revision_id INTEGER REFERENCES dat_revisions (id),
    pack_sha256 TEXT,
    release TEXT,
    asset_url TEXT
);
CREATE INDEX fetches_artifact ON fetches (provider, system, artifact, id);
