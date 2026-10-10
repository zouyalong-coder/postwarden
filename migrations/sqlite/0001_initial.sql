CREATE TABLE admin_users (
    id TEXT PRIMARY KEY NOT NULL,
    singleton INTEGER NOT NULL DEFAULT 1 UNIQUE CHECK (singleton = 1),
    login_name TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE admin_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    admin_id TEXT NOT NULL REFERENCES admin_users(id) ON DELETE CASCADE,
    session_digest BLOB NOT NULL UNIQUE,
    expires_at TEXT NOT NULL,
    revoked_at TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_admin_sessions_admin_expires ON admin_sessions(admin_id, expires_at);

CREATE TABLE smtp_accounts (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    host TEXT NOT NULL,
    port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    tls_mode TEXT NOT NULL CHECK (tls_mode IN ('implicit', 'starttls', 'none')),
    auth_type TEXT NOT NULL CHECK (auth_type IN ('none', 'login', 'plain')),
    username TEXT,
    credential_ciphertext BLOB,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    deleted_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (auth_type = 'none' OR (username IS NOT NULL AND credential_ciphertext IS NOT NULL)),
    CHECK (auth_type = 'none' OR tls_mode <> 'none')
);
CREATE INDEX idx_smtp_accounts_active ON smtp_accounts(enabled, deleted_at);

CREATE TABLE sender_identities (
    id TEXT PRIMARY KEY NOT NULL,
    smtp_account_id TEXT NOT NULL REFERENCES smtp_accounts(id) ON DELETE RESTRICT,
    display_name TEXT NOT NULL,
    email_address TEXT NOT NULL,
    envelope_from TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    deleted_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_sender_identities_account ON sender_identities(smtp_account_id, enabled, deleted_at);

CREATE TABLE api_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    purpose TEXT,
    lookup_prefix TEXT NOT NULL,
    secret_digest BLOB NOT NULL UNIQUE,
    status TEXT NOT NULL CHECK (status IN ('enabled', 'disabled', 'revoked')),
    expires_at TEXT,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
);
CREATE INDEX idx_api_tokens_status_expiry ON api_tokens(status, expires_at);

CREATE TABLE token_sender_permissions (
    token_id TEXT NOT NULL REFERENCES api_tokens(id) ON DELETE CASCADE,
    sender_identity_id TEXT NOT NULL REFERENCES sender_identities(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (token_id, sender_identity_id)
);
CREATE INDEX idx_token_sender_permissions_identity ON token_sender_permissions(sender_identity_id);

CREATE TABLE messages (
    id TEXT PRIMARY KEY NOT NULL,
    token_id TEXT NOT NULL REFERENCES api_tokens(id) ON DELETE RESTRICT,
    sender_identity_id TEXT NOT NULL REFERENCES sender_identities(id) ON DELETE RESTRICT,
    identity_version INTEGER NOT NULL CHECK (identity_version > 0),
    mode TEXT NOT NULL CHECK (mode IN ('sync', 'async')),
    status TEXT NOT NULL CHECK (status IN ('queued', 'processing', 'retry_wait', 'held', 'smtp_accepted', 'rejected', 'failed', 'outcome_unknown', 'expired')),
    idempotency_key TEXT NOT NULL,
    request_digest BLOB NOT NULL,
    recipient_ciphertext BLOB NOT NULL,
    created_at TEXT NOT NULL,
    queued_at TEXT,
    expires_at TEXT,
    finished_at TEXT,
    next_attempt_at TEXT,
    last_error_code TEXT,
    content_retained_until TEXT,
    audit_retained_until TEXT,
    UNIQUE (token_id, idempotency_key)
);
CREATE INDEX idx_messages_status_next_attempt ON messages(status, next_attempt_at);
CREATE INDEX idx_messages_created ON messages(created_at DESC);
CREATE INDEX idx_messages_token_created ON messages(token_id, created_at DESC);
CREATE INDEX idx_messages_sender_created ON messages(sender_identity_id, created_at DESC);
CREATE INDEX idx_messages_audit_retention ON messages(audit_retained_until);
CREATE INDEX idx_messages_content_retention ON messages(status, content_retained_until);

CREATE TABLE message_payloads (
    message_id TEXT PRIMARY KEY NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    payload_ciphertext BLOB NOT NULL,
    key_version INTEGER NOT NULL CHECK (key_version > 0),
    retained_until TEXT
);
CREATE INDEX idx_message_payloads_retained_until ON message_payloads(retained_until);

CREATE TABLE message_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    attempt_no INTEGER NOT NULL CHECK (attempt_no > 0),
    started_at TEXT NOT NULL,
    finished_at TEXT,
    smtp_stage TEXT,
    result TEXT CHECK (result IN ('accepted', 'temporary_failure', 'permanent_failure', 'unknown')),
    smtp_code INTEGER CHECK (smtp_code IS NULL OR smtp_code BETWEEN 100 AND 599),
    safe_error_code TEXT,
    UNIQUE (message_id, attempt_no)
);
CREATE INDEX idx_message_attempts_message_started ON message_attempts(message_id, started_at);

CREATE TABLE message_events (
    id TEXT PRIMARY KEY NOT NULL,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    plugin_id TEXT,
    created_at TEXT NOT NULL,
    safe_details TEXT
);
CREATE INDEX idx_message_events_message_created ON message_events(message_id, created_at);
CREATE INDEX idx_message_events_created ON message_events(created_at);

CREATE TABLE plugin_configs (
    plugin_id TEXT PRIMARY KEY NOT NULL,
    plugin_version TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    execution_order INTEGER NOT NULL,
    config_ciphertext BLOB,
    updated_at TEXT NOT NULL
);

CREATE TABLE plugin_message_states (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    plugin_id TEXT NOT NULL,
    plugin_version TEXT NOT NULL,
    state_ciphertext BLOB NOT NULL,
    resume_at TEXT,
    PRIMARY KEY (message_id, plugin_id)
);
CREATE INDEX idx_plugin_message_states_resume ON plugin_message_states(resume_at);
