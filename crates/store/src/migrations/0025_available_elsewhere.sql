-- A Game counts only its own files (#125, ADR 0017).

-- A JSON array of the Game's Available Elsewhere ROMs, each with the Games whose ZIP or folder holds it. An earlier
-- Audit counted a ROM matched inside another Game's ZIP or folder as present, and keeps the statuses it found until
-- the Source Set is audited again: it has none.
ALTER TABLE audit_games ADD COLUMN available_elsewhere TEXT NOT NULL DEFAULT '[]';
