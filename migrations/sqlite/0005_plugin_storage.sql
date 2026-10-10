CREATE TABLE plugin_storage (
    plugin_id TEXT NOT NULL,
    storage_key TEXT NOT NULL,
    encrypted_value BLOB NOT NULL,
    PRIMARY KEY (plugin_id, storage_key)
);
