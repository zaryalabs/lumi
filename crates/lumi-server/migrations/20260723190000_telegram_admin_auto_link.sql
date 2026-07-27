-- Bind the instance-wide Telegram bot directly to the administrator who configured it.

ALTER TABLE telegram_bot_settings
    ADD COLUMN configured_by_device_id uuid
        REFERENCES sync_devices(device_id) ON DELETE SET NULL;

UPDATE telegram_bot_settings AS settings
SET configured_by_device_id = (
    SELECT devices.device_id
    FROM sync_devices AS devices
    WHERE devices.user_id = settings.configured_by_user_id
      AND devices.revoked_at IS NULL
    ORDER BY devices.last_seen_at DESC
    LIMIT 1
)
WHERE settings.configured_by_user_id IS NOT NULL;
