CREATE TABLE plugin_storage (
    plugin_id VARCHAR(255) NOT NULL,
    storage_key VARCHAR(255) NOT NULL,
    encrypted_value LONGBLOB NOT NULL,
    PRIMARY KEY (plugin_id, storage_key)
) ENGINE=InnoDB;
