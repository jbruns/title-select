-- Each ingest item shows its latest move and the Job carrying it out (#76): find an item's moves, and what a move
-- became, without reading every move.
CREATE INDEX ingest_moves_by_item ON ingest_moves (item_id, id) WHERE item_id IS NOT NULL;
CREATE INDEX repair_steps_by_move ON repair_steps (move_id) WHERE move_id IS NOT NULL;
CREATE INDEX additions_by_move ON additions (move_id) WHERE move_id IS NOT NULL;
CREATE INDEX jobs_by_repair_plan ON jobs (repair_plan_id) WHERE repair_plan_id IS NOT NULL;
