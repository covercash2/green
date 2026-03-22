ALTER TABLE mqtt_devices ADD COLUMN pattern TEXT NOT NULL DEFAULT '';

ALTER TABLE mqtt_devices
    DROP CONSTRAINT mqtt_devices_integration_device_id_key;

ALTER TABLE mqtt_devices
    ADD CONSTRAINT mqtt_devices_integration_pattern_device_id_key
    UNIQUE (integration, pattern, device_id);
