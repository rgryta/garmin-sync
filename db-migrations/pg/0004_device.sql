-- The account's registered devices, from Garmin's device registry. `id` is Garmin's
-- deviceId: the same number is on each activity (activity.device) and is the serial in
-- the device's own FIT files, so it links every record to the watch that made it. The
-- registry names the exact model (a quatix and an epix can share one FIT product id).
CREATE TABLE device (
  id              bigint PRIMARY KEY,
  display_name    text,
  product_sku     text,
  part_number     text,
  application_key text,
  registered_at   timestamptz,
  raw             jsonb NOT NULL,
  updated_at      timestamptz NOT NULL DEFAULT now()
);

-- The device that wrote each wellness file and reading, from the FIT files' serial.
ALTER TABLE wellness_file ADD COLUMN device_id bigint;
ALTER TABLE wellness_sample ADD COLUMN device_id bigint;

DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM pg_tablespace WHERE spcname = 'archive_data')
     AND EXISTS (SELECT 1 FROM pg_tablespace WHERE spcname = 'nvme_indexes') THEN
    ALTER TABLE device SET TABLESPACE archive_data;
    ALTER INDEX device_pkey SET TABLESPACE nvme_indexes;
  END IF;
END $$;
