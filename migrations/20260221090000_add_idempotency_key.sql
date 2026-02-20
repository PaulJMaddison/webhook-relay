ALTER TABLE events
    ADD COLUMN IF NOT EXISTS idempotency_key TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS events_source_idempotency_key_uidx
    ON events (source, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
