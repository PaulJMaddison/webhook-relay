ALTER TABLE events
    ADD COLUMN IF NOT EXISTS body_size_bytes INT;

UPDATE events
SET body_size_bytes = octet_length(body)
WHERE body_size_bytes IS NULL;

ALTER TABLE events
    ALTER COLUMN body_size_bytes SET NOT NULL;
