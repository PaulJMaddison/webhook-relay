CREATE INDEX IF NOT EXISTS events_received_at_id_desc_idx
    ON events (received_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS events_source_received_at_idx
    ON events (source, received_at DESC);

CREATE INDEX IF NOT EXISTS events_status_received_at_idx
    ON events (status, received_at DESC);
