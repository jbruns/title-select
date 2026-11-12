-- Headers (#102): a DAT's Header Rule matches a dump with or without its Header (ADR 0016).

-- The Header Rule the Audit's DAT Revision named, as the DAT names it; null when it names none. Kept per Audit, so
-- earlier Audits need nothing: their DAT Revisions were audited as naming none.
ALTER TABLE audits ADD COLUMN header_rule TEXT;
-- How many of the Audit's Games carry each Game Warning, kept like the other counts once the Audit is pruned.
ALTER TABLE audits ADD COLUMN header_missing INTEGER NOT NULL DEFAULT 0;

-- For Payload Evidence, how many bytes of Header the Audit left out of the Entry's checksums; null when none.
ALTER TABLE audit_entries ADD COLUMN header_bytes INTEGER;
-- A JSON array of the Game's Game Warnings.
ALTER TABLE audit_games ADD COLUMN warnings TEXT NOT NULL DEFAULT '[]';

-- The Header Rule a payload read was hashed under; null for none. A later Audit reuses the read only under the same
-- rule. Every earlier read hashed whole payloads.
ALTER TABLE payload_reads ADD COLUMN header_rule TEXT;
