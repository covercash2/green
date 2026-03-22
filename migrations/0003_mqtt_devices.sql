CREATE TABLE mqtt_devices (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    integration   TEXT        NOT NULL,
    device_id     TEXT        NOT NULL,
    first_seen    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    message_count BIGINT      NOT NULL DEFAULT 1,
    UNIQUE (integration, device_id)
);
