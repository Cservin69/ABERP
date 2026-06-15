-- S429 — closed-loop calibration link. Two additive nullable columns on
-- work_orders so a WO can (a) carry the auto-quote it originated from and
-- (b) record the actual machining time spent making it. Both are NULL for
-- operator-authored WOs with no quote origin / no time tracking — the
-- calibration sample hook (app layer) skips those.
--
-- Additive, forward-only, no CHECK / no backfill — same idempotent posture
-- as V001 (re-running against a migrated tenant is a no-op via IF NOT EXISTS).
ALTER TABLE work_orders ADD COLUMN IF NOT EXISTS source_quote_id VARCHAR;
ALTER TABLE work_orders ADD COLUMN IF NOT EXISTS actual_machining_minutes DOUBLE;
