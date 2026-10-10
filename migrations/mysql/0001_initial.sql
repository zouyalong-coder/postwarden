CREATE TABLE admin_users (
    id VARCHAR(36) NOT NULL PRIMARY KEY,
    singleton TINYINT NOT NULL DEFAULT 1 UNIQUE,
    login_name VARCHAR(191) NOT NULL UNIQUE,
    password_hash VARCHAR(255) NOT NULL,
    created_at DATETIME(6) NOT NULL,
    updated_at DATETIME(6) NOT NULL,
    CONSTRAINT chk_admin_users_singleton CHECK (singleton = 1)
) ENGINE=InnoDB;

CREATE TABLE admin_sessions (
    id VARCHAR(36) NOT NULL PRIMARY KEY,
    admin_id VARCHAR(36) NOT NULL,
    session_digest BINARY(32) NOT NULL UNIQUE,
    expires_at DATETIME(6) NOT NULL,
    revoked_at DATETIME(6) NULL,
    created_at DATETIME(6) NOT NULL,
    CONSTRAINT fk_admin_sessions_admin FOREIGN KEY (admin_id) REFERENCES admin_users(id) ON DELETE CASCADE,
    INDEX idx_admin_sessions_admin_expires (admin_id, expires_at)
) ENGINE=InnoDB;

CREATE TABLE smtp_accounts (
    id VARCHAR(36) NOT NULL PRIMARY KEY,
    name VARCHAR(191) NOT NULL,
    host VARCHAR(255) NOT NULL,
    port SMALLINT UNSIGNED NOT NULL,
    tls_mode VARCHAR(16) NOT NULL,
    auth_type VARCHAR(16) NOT NULL,
    username VARCHAR(320) NULL,
    credential_ciphertext LONGBLOB NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    deleted_at DATETIME(6) NULL,
    created_at DATETIME(6) NOT NULL,
    updated_at DATETIME(6) NOT NULL,
    CONSTRAINT chk_smtp_accounts_tls CHECK (tls_mode IN ('implicit', 'starttls', 'none')),
    CONSTRAINT chk_smtp_accounts_auth CHECK (auth_type IN ('none', 'login', 'plain')),
    CONSTRAINT chk_smtp_accounts_auth_credentials CHECK (auth_type = 'none' OR (username IS NOT NULL AND credential_ciphertext IS NOT NULL)),
    CONSTRAINT chk_smtp_accounts_auth_tls CHECK (auth_type = 'none' OR tls_mode <> 'none'),
    INDEX idx_smtp_accounts_active (enabled, deleted_at)
) ENGINE=InnoDB;

CREATE TABLE sender_identities (
    id VARCHAR(36) NOT NULL PRIMARY KEY,
    smtp_account_id VARCHAR(36) NOT NULL,
    display_name VARCHAR(191) NOT NULL,
    email_address VARCHAR(320) NOT NULL,
    envelope_from VARCHAR(320) NOT NULL,
    version BIGINT UNSIGNED NOT NULL DEFAULT 1,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    deleted_at DATETIME(6) NULL,
    created_at DATETIME(6) NOT NULL,
    updated_at DATETIME(6) NOT NULL,
    CONSTRAINT fk_sender_identities_account FOREIGN KEY (smtp_account_id) REFERENCES smtp_accounts(id) ON DELETE RESTRICT,
    INDEX idx_sender_identities_account (smtp_account_id, enabled, deleted_at)
) ENGINE=InnoDB;

CREATE TABLE api_tokens (
    id VARCHAR(36) NOT NULL PRIMARY KEY,
    name VARCHAR(191) NOT NULL,
    purpose VARCHAR(255) NULL,
    lookup_prefix VARCHAR(64) NOT NULL,
    secret_digest BINARY(32) NOT NULL UNIQUE,
    status VARCHAR(16) NOT NULL,
    expires_at DATETIME(6) NULL,
    created_at DATETIME(6) NOT NULL,
    last_used_at DATETIME(6) NULL,
    revoked_at DATETIME(6) NULL,
    CONSTRAINT chk_api_tokens_status CHECK (status IN ('enabled', 'disabled', 'revoked')),
    INDEX idx_api_tokens_status_expiry (status, expires_at)
) ENGINE=InnoDB;

CREATE TABLE token_sender_permissions (
    token_id VARCHAR(36) NOT NULL,
    sender_identity_id VARCHAR(36) NOT NULL,
    created_at DATETIME(6) NOT NULL,
    PRIMARY KEY (token_id, sender_identity_id),
    CONSTRAINT fk_token_sender_permissions_token FOREIGN KEY (token_id) REFERENCES api_tokens(id) ON DELETE CASCADE,
    CONSTRAINT fk_token_sender_permissions_identity FOREIGN KEY (sender_identity_id) REFERENCES sender_identities(id) ON DELETE CASCADE,
    INDEX idx_token_sender_permissions_identity (sender_identity_id)
) ENGINE=InnoDB;

CREATE TABLE messages (
    id VARCHAR(36) NOT NULL PRIMARY KEY,
    token_id VARCHAR(36) NOT NULL,
    sender_identity_id VARCHAR(36) NOT NULL,
    identity_version BIGINT UNSIGNED NOT NULL,
    mode VARCHAR(8) NOT NULL,
    status VARCHAR(24) NOT NULL,
    idempotency_key VARCHAR(255) NOT NULL,
    request_digest BINARY(32) NOT NULL,
    recipient_ciphertext LONGBLOB NOT NULL,
    created_at DATETIME(6) NOT NULL,
    queued_at DATETIME(6) NULL,
    expires_at DATETIME(6) NULL,
    finished_at DATETIME(6) NULL,
    next_attempt_at DATETIME(6) NULL,
    last_error_code VARCHAR(96) NULL,
    content_retained_until DATETIME(6) NULL,
    audit_retained_until DATETIME(6) NULL,
    CONSTRAINT fk_messages_token FOREIGN KEY (token_id) REFERENCES api_tokens(id) ON DELETE RESTRICT,
    CONSTRAINT fk_messages_sender_identity FOREIGN KEY (sender_identity_id) REFERENCES sender_identities(id) ON DELETE RESTRICT,
    CONSTRAINT chk_messages_mode CHECK (mode IN ('sync', 'async')),
    CONSTRAINT chk_messages_status CHECK (status IN ('queued', 'processing', 'retry_wait', 'held', 'smtp_accepted', 'rejected', 'failed', 'outcome_unknown', 'expired')),
    UNIQUE KEY uq_messages_token_idempotency (token_id, idempotency_key),
    INDEX idx_messages_status_next_attempt (status, next_attempt_at),
    INDEX idx_messages_created (created_at),
    INDEX idx_messages_token_created (token_id, created_at),
    INDEX idx_messages_sender_created (sender_identity_id, created_at),
    INDEX idx_messages_audit_retention (audit_retained_until),
    INDEX idx_messages_content_retention (status, content_retained_until)
) ENGINE=InnoDB;

CREATE TABLE message_payloads (
    message_id VARCHAR(36) NOT NULL PRIMARY KEY,
    payload_ciphertext LONGBLOB NOT NULL,
    key_version INT UNSIGNED NOT NULL,
    retained_until DATETIME(6) NULL,
    CONSTRAINT fk_message_payloads_message FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE,
    INDEX idx_message_payloads_retained_until (retained_until)
) ENGINE=InnoDB;

CREATE TABLE message_attempts (
    id VARCHAR(36) NOT NULL PRIMARY KEY,
    message_id VARCHAR(36) NOT NULL,
    attempt_no INT UNSIGNED NOT NULL,
    started_at DATETIME(6) NOT NULL,
    finished_at DATETIME(6) NULL,
    smtp_stage VARCHAR(32) NULL,
    result VARCHAR(24) NULL,
    smtp_code SMALLINT UNSIGNED NULL,
    safe_error_code VARCHAR(96) NULL,
    CONSTRAINT fk_message_attempts_message FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE,
    CONSTRAINT chk_message_attempts_result CHECK (result IN ('accepted', 'temporary_failure', 'permanent_failure', 'unknown')),
    CONSTRAINT chk_message_attempts_smtp_code CHECK (smtp_code IS NULL OR smtp_code BETWEEN 100 AND 599),
    UNIQUE KEY uq_message_attempts_number (message_id, attempt_no),
    INDEX idx_message_attempts_message_started (message_id, started_at)
) ENGINE=InnoDB;

CREATE TABLE message_events (
    id VARCHAR(36) NOT NULL PRIMARY KEY,
    message_id VARCHAR(36) NOT NULL,
    event_type VARCHAR(64) NOT NULL,
    plugin_id VARCHAR(191) NULL,
    created_at DATETIME(6) NOT NULL,
    safe_details TEXT NULL,
    CONSTRAINT fk_message_events_message FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE,
    INDEX idx_message_events_message_created (message_id, created_at),
    INDEX idx_message_events_created (created_at)
) ENGINE=InnoDB;

CREATE TABLE plugin_configs (
    plugin_id VARCHAR(191) NOT NULL PRIMARY KEY,
    plugin_version VARCHAR(64) NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    execution_order INT NOT NULL,
    config_ciphertext LONGBLOB NULL,
    updated_at DATETIME(6) NOT NULL
) ENGINE=InnoDB;

CREATE TABLE plugin_message_states (
    message_id VARCHAR(36) NOT NULL,
    plugin_id VARCHAR(191) NOT NULL,
    plugin_version VARCHAR(64) NOT NULL,
    state_ciphertext LONGBLOB NOT NULL,
    resume_at DATETIME(6) NULL,
    PRIMARY KEY (message_id, plugin_id),
    CONSTRAINT fk_plugin_message_states_message FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE,
    INDEX idx_plugin_message_states_resume (resume_at)
) ENGINE=InnoDB;
