# garmin-sync

A permanent local archive of all Garmin Connect data, and a read-only MCP server over
it. Two binaries, one scratch image:

- `garmin-ingest` reads the Garmin app's private API and writes the archive in
  PostgreSQL. It backfills the whole history and keeps it current.
- `garmin-mcp` serves the archive to MCP clients. It holds no credential and never
  reaches Garmin.

Every activity is archived with its summary, its original FIT file (`activity_file`), and
its per-second samples (`activity_sample`: heart rate, cadence, power, speed, altitude,
GPS, temperature). Daily tracking metrics (sleep, HRV, resting heart rate, stress, body
battery, steps, respiration, SpO2, training readiness, VO2max, weight) are kept per day
in `daily_metric`. Each day's wellness download (the zipped FIT files the watch synced)
is kept in `wellness_file`, and its intraday readings in `wellness_sample`: heart rate
about every minute, HRV every 5 minutes overnight, stress and respiration about every
minute. Garmin's JSON drops the intraday heart rate and HRV of older days; these files
keep it. The account's device registry is kept in `device`, refreshed with the daily
metrics: Garmin's deviceId there is the `device` on each activity and the serial in the
watch's FIT files (`device_id` on wellness rows), so every record names the exact model
that made it. The ingester keeps its OAuth session in the `ingest_session` table,
which the MCP role cannot read.

## Design notes

- **No `#` comments in `deploy/compose.yaml`.** Rationale lives here, not in the compose
  file, by house convention.
- **Raw-first storage.** Every response is kept verbatim in `raw_response`; the typed
  tables (`activity`, `activity_sample`, `daily_metric`) are projections for the MCP to
  query. Nothing is lost when Garmin adds a field.
- **A bad FIT keeps the activity.** If an activity's FIT will not parse, the activity row
  and the raw FIT blob are still stored; only the per-second samples are skipped, so the
  file can be re-parsed later.
- **Seed-once auth.** The owner signs in once (the interactive, Cloudflare-guarded SSO);
  the resulting OAuth2 refresh token is seeded into `ingest_session`, and the ingester
  mints and rolls its own tokens from then on. It never logs in and never holds the
  password. Cloudflare guards only the SSO login, not the API the ingester uses.

## Seeding the session

The ingester never does the interactive sign-in. Seed it once with the **OAuth2 refresh
token** a signed-in Garmin mobile client holds — it rolls on each use but does not
otherwise expire, so the ingester re-authenticates itself from then on (via the `diauth`
`grant_type=refresh_token` exchange):

1. Capture a session JSON with the rolling refresh token and the public client id:
   `{ "refresh_token": "...", "client_id": "GARMIN_CONNECT_MOBILE_ANDROID_DI_2025Q2" }`.
   The optional `oauth1_token`/`oauth1_secret`/`garmin_user_id` are filled in on the
   first refresh if absent.
2. Put it on the host and point the ingester at it for one start with
   `GARMIN_SESSION_FROM=/path/to/session.json`. It is written to `ingest_session` and the
   database wins from then on; remove the env var and file afterwards.

To replace a dead refresh token, delete the `ingest_session` row and seed again.

## Deploy

`deploy/provision-garmin-db.sql` creates the `garmin` database and its two roles
(`garmin_ingest` read-write, `garmin_mcp` read-only). `deploy/compose.yaml` runs both
services on `homelab-network`; `deploy/Dockerfile` builds the static musl image.
