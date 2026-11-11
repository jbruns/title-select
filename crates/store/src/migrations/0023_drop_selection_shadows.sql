-- The cutover to the one Selection engine (ADR 0015) ends shadow mode: the shadow runs recorded beside Selections
-- compared an engine that is gone, so they are dropped.

ALTER TABLE selections DROP COLUMN shadow;
