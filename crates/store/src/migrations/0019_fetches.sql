-- Fetching (ADR 0012): every request to a DAT Provider, recorded before it is sent and finished with its outcome.
-- The latest request of each system and artifact holds the next back for a day, whatever its outcome.

-- A cue or SBI pack a fetch brought, kept whole in the store directory under packs/<sha256>.
CREATE TABLE fetched_packs (
    sha256 TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    size INTEGER NOT NULL,
    stored_at TEXT NOT NULL
);

-- system: Redump's system code, or for MAME `mame` (its release and ListXML) or a software list's name.
-- outcome: added, unchanged or failed; absent while the request runs, or if the Service stopped during it.
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
    pack_sha256 TEXT REFERENCES fetched_packs (sha256),
    release TEXT,
    asset_url TEXT
);
CREATE INDEX fetches_artifact ON fetches (provider, system, artifact, id);
