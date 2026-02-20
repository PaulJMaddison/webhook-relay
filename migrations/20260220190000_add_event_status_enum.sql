DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'event_status') THEN
        CREATE TYPE event_status AS ENUM ('received', 'delivered', 'failed');
    END IF;
END $$;

ALTER TABLE events
    ALTER COLUMN status DROP DEFAULT,
    ALTER COLUMN status TYPE event_status
    USING CASE
        WHEN status = 'received' THEN 'received'::event_status
        WHEN status = 'delivered' THEN 'delivered'::event_status
        WHEN status = 'failed' THEN 'failed'::event_status
        ELSE 'failed'::event_status
    END,
    ALTER COLUMN status SET DEFAULT 'received'::event_status;
