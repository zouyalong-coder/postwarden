CREATE TABLE notification_templates (
    id VARCHAR(36) NOT NULL PRIMARY KEY,
    scenario_id VARCHAR(191) NOT NULL UNIQUE,
    name VARCHAR(191) NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    allow_caller_recipients BOOLEAN NOT NULL DEFAULT FALSE,
    sender_identity_id VARCHAR(36) NOT NULL,
    subject_template TEXT NOT NULL,
    text_template MEDIUMTEXT NULL,
    html_template MEDIUMTEXT NULL,
    to_json JSON NOT NULL,
    cc_json JSON NOT NULL,
    bcc_json JSON NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    created_at DATETIME(6) NOT NULL,
    updated_at DATETIME(6) NOT NULL,
    CONSTRAINT fk_notification_templates_identity FOREIGN KEY (sender_identity_id) REFERENCES sender_identities(id) ON DELETE RESTRICT,
    INDEX idx_notification_templates_enabled (scenario_id, enabled)
) ENGINE=InnoDB;
