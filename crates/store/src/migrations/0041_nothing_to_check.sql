-- Nothing to Check (#231, ADR 0025): a fourth Game Status, for a Game that lists no ROM that must be present and can
-- match. An Audit counts its Games of it apart from the Complete, Incomplete and Missing counts.
ALTER TABLE audits ADD COLUMN nothing_to_check INTEGER NOT NULL DEFAULT 0;
-- How many of its Nothing to Check Games it found any of their own files of: what a placed count counts against.
ALTER TABLE audits ADD COLUMN nothing_to_check_found INTEGER NOT NULL DEFAULT 0;
