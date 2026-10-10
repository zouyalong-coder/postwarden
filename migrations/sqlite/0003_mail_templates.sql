CREATE TABLE notification_templates (
    id TEXT PRIMARY KEY NOT NULL,
    scenario_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    allow_caller_recipients INTEGER NOT NULL DEFAULT 0 CHECK (allow_caller_recipients IN (0, 1)),
    sender_identity_id TEXT NOT NULL REFERENCES sender_identities(id) ON DELETE RESTRICT,
    subject_template TEXT NOT NULL,
    text_template TEXT,
    html_template TEXT,
    to_json TEXT NOT NULL,
    cc_json TEXT NOT NULL DEFAULT '[]',
    bcc_json TEXT NOT NULL DEFAULT '[]',
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_notification_templates_enabled ON notification_templates(scenario_id, enabled);
