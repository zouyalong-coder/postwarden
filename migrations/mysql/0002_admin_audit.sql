CREATE TABLE admin_audit_events (
    id VARCHAR(36) NOT NULL PRIMARY KEY,
    admin_id VARCHAR(36) NOT NULL,
    action VARCHAR(64) NOT NULL,
    resource_id VARCHAR(36) NOT NULL,
    outcome VARCHAR(32) NOT NULL,
    created_at DATETIME(6) NOT NULL,
    CONSTRAINT fk_admin_audit_events_admin FOREIGN KEY (admin_id) REFERENCES admin_users(id) ON DELETE RESTRICT,
    INDEX idx_admin_audit_events_created (created_at),
    INDEX idx_admin_audit_events_admin_created (admin_id, created_at)
) ENGINE=InnoDB;
