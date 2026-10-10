CREATE TABLE admin_audit_events (
    id TEXT PRIMARY KEY NOT NULL,
    admin_id TEXT NOT NULL REFERENCES admin_users(id) ON DELETE RESTRICT,
    action TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    outcome TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_admin_audit_events_created ON admin_audit_events(created_at DESC);
CREATE INDEX idx_admin_audit_events_admin_created ON admin_audit_events(admin_id, created_at DESC);
