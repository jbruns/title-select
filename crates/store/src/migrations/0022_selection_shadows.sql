-- Shadow mode: a Selection Job can also run the other Selection engine and record how its Selection compared, as
-- JSON (ShadowRun). It never changes the Selection.

ALTER TABLE selections ADD COLUMN shadow TEXT;
