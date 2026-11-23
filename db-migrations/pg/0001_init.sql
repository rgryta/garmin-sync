-- The Garmin Connect archive schema.
--
-- Raw-first: every response is kept, and the typed tables are projections for the MCP to
-- query. Built for volume: years of activities, each with a FIT blob and per-second
-- samples (millions of rows), plus ~20 daily metrics every day. The large tables go on
-- the archive tablespace (migration 0002), their indexes on NVMe.

-- The account the session belongs to, and where an auth failure is recorded so the MCP
-- `status` tool can report it without reaching Garmin.
CREATE TABLE account (
  id                 text PRIMARY KEY,
  garmin_user_id     text,
  display_name       text,
  last_auth_error    text,
  last_auth_error_at timestamptz,
  updated_at         timestamptz NOT NULL DEFAULT now()
);

-- The ingester's Garmin session. Holds the rolling OAuth2 refresh token and the public
-- client_id the ingester re-authenticates with (`diauth .../oauth/token`, grant_type=
-- refresh_token), the current access token and its expiry, and an optional long-lived
-- OAuth1 token/secret fallback. No 2FA or interactive SSO after the one-time seed. The
-- MCP process must never hold a session; the REVOKE below is the whole point.
CREATE TABLE ingest_session (
  account_id text        PRIMARY KEY,
  session    jsonb       NOT NULL,
  updated_at timestamptz NOT NULL DEFAULT now()
);
DO $$ BEGIN
  EXECUTE 'REVOKE ALL ON ingest_session FROM garmin_mcp';
EXCEPTION WHEN undefined_object THEN NULL;
END $$;

-- One activity (a workout you did). `manual` distinguishes a manually-started activity
-- from an auto-detected one; `parent_id` links a multisport session's children. The thin
-- typed columns are for listing and filtering; the whole summary stays in `summary`.
CREATE TABLE activity (
  id          bigint PRIMARY KEY,
  parent_id   bigint,
  start_ts    timestamptz NOT NULL,
  type        text,
  name        text,
  manual      boolean,
  duration_s  double precision,
  distance_m  double precision,
  device      text,
  deleted_at  timestamptz,
  summary     jsonb NOT NULL,
  updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX activity_start ON activity (start_ts DESC);
CREATE INDEX activity_type ON activity (type);

-- The original uploaded file for an activity (FIT, or whatever the upload was), unzipped.
-- Kept whole so nothing Garmin strips from the API-delivered copy is lost downstream.
CREATE TABLE activity_file (
  activity_id bigint PRIMARY KEY REFERENCES activity (id) ON DELETE CASCADE,
  format      text,
  bytes       bytea NOT NULL,
  sha256      text,
  fetched_at  timestamptz NOT NULL DEFAULT now()
);

-- One per-second (or per-record) sample parsed from the FIT file. Expect millions of
-- rows; the (activity_id, ts) key makes a re-parse idempotent, and a BRIN index on
-- activity_id keeps the index tiny on append-only data. Lives on the archive tablespace.
CREATE TABLE activity_sample (
  activity_id bigint NOT NULL REFERENCES activity (id) ON DELETE CASCADE,
  ts          timestamptz NOT NULL,
  elapsed_s   int,
  heart_rate  int,
  cadence     double precision,
  power       int,
  speed       double precision,
  altitude    double precision,
  distance_m  double precision,
  lat         double precision,
  lon         double precision,
  temperature double precision,
  extra       jsonb,
  PRIMARY KEY (activity_id, ts)
);
CREATE INDEX activity_sample_activity_brin ON activity_sample USING brin (activity_id);

-- The raw-first daily store: one row per (kind, date) holding Garmin's verbatim response.
-- Kinds: sleep, hrv, stress, body_battery, steps, summary, respiration, spo2,
-- heart_rate, readiness, training_status, vo2max, weight, blood_pressure, ... Garmin
-- recomputes some days later, so the poll overwrites a trailing window.
CREATE TABLE daily_metric (
  kind       text NOT NULL,
  date       date NOT NULL,
  raw        jsonb NOT NULL,
  fetched_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (kind, date)
);

-- Everything else, verbatim, keyed by endpoint kind + a natural key: activity detail,
-- laps, weather and zones (key = activity id); account-level kinds (devices, gear,
-- records, badges, workouts, training plan) with a fixed key.
CREATE TABLE raw_response (
  kind       text NOT NULL,
  key        text NOT NULL,
  fetched_at timestamptz NOT NULL DEFAULT now(),
  body       jsonb NOT NULL,
  PRIMARY KEY (kind, key)
);

-- One row per loop: last run, the daily request budget (counted here so a restart does
-- not hand out a fresh allowance), and the backfill floor.
CREATE TABLE sync_state (
  loop           text PRIMARY KEY,
  last_run_at    timestamptz,
  last_ok_at     timestamptz,
  last_error     text,
  budget_day     date,
  budget_spent   bigint NOT NULL DEFAULT 0,
  backfill_floor timestamptz,
  backfill_done  boolean NOT NULL DEFAULT false
);

-- Per-kind backfill progress: the activities page cursor, and each daily kind's
-- backfilled-through date. `done` when that kind has reached the floor.
CREATE TABLE backfill_cursor (
  kind   text PRIMARY KEY,
  cursor text,
  done   boolean NOT NULL DEFAULT false
);
