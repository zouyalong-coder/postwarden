RENAME TABLE notification_templates TO mail_templates;
ALTER TABLE mail_templates
    DROP INDEX idx_notification_templates_enabled,
    ADD INDEX idx_mail_templates_enabled (scenario_id, enabled);
