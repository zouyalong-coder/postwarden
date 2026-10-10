ALTER TABLE notification_templates RENAME TO mail_templates;
DROP INDEX idx_notification_templates_enabled;
CREATE INDEX idx_mail_templates_enabled ON mail_templates(scenario_id, enabled);
