-- Multi-disc layout (#100): how a disc system's Play Libraries lay out its discs (ADR 0014), and whether they put each
-- disc's SBI file beside it. A scope's default row with no layout means the ES-DE layout; a system's row with none
-- follows that default. Only a system's row opts in to SBI files.
ALTER TABLE storage_policy ADD COLUMN layout TEXT CHECK (layout IN ('es_de', 'flat'));
ALTER TABLE storage_policy ADD COLUMN sbi INTEGER CHECK (sbi IN (0, 1));
