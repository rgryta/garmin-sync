-- The day's wellness download: a zip of the FIT files the watch synced that day
-- (monitoring, HRV, stress, respiration, sleep). Kept whole, so a later parser can read
-- more of it; Garmin's JSON API drops the intraday detail of older days, these files keep it.
CREATE TABLE wellness_file (
  date       date PRIMARY KEY,
  bytes      bytea NOT NULL,
  sha256     text,
  fetched_at timestamptz NOT NULL DEFAULT now()
);

-- Intraday readings parsed from those files. Kinds: heart_rate (bpm, about every minute),
-- hrv (ms, every 5 minutes overnight), stress (0-100, about every minute), respiration
-- (breaths per minute, about every minute).
CREATE TABLE wellness_sample (
  kind  text NOT NULL,
  ts    timestamptz NOT NULL,
  value double precision NOT NULL,
  PRIMARY KEY (kind, ts)
);

-- Same placement as 0002: data on the archive tablespace, indexes on NVMe, when present.
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM pg_tablespace WHERE spcname = 'archive_data')
     AND EXISTS (SELECT 1 FROM pg_tablespace WHERE spcname = 'nvme_indexes') THEN
    ALTER TABLE wellness_file SET TABLESPACE archive_data;
    ALTER TABLE wellness_sample SET TABLESPACE archive_data;
    ALTER INDEX wellness_file_pkey SET TABLESPACE nvme_indexes;
    ALTER INDEX wellness_sample_pkey SET TABLESPACE nvme_indexes;
  END IF;
END $$;
