-- What an Audit found, as a SHA-256 over its DAT Revision, Entries and Game Statuses, so that a Library Folder is
-- stale only when a newer Audit of its Source Set found something its Build's did not (#75). NULL for an Audit
-- finished before this; it is worked out when needed.
ALTER TABLE audits ADD COLUMN content_sha256 TEXT;
