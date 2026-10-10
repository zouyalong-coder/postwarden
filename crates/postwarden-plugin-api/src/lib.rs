//! Stable, serializable interfaces for PostWarden build-time plugins.
//!
//! Plugins are trusted Rust code linked into the service at build time. The
//! host must validate every modified draft before handing it to an SMTP client.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{error::Error, fmt, sync::Arc};

pub use async_trait::async_trait;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Mailbox {
    pub name: Option<String>,
    pub address: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "mailbox", rename_all = "snake_case")]
pub enum Recipient {
    To(Mailbox),
    Cc(Mailbox),
    Bcc(Mailbox),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    /// Base64-encoded attachment bytes, suitable for JSON persistence.
    pub data_base64: String,
    pub inline_content_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct MailDraft {
    /// Sender identity selected by the API caller. The host must revalidate it
    /// after every plugin modification.
    pub sender_identity_id: String,
    pub from: Option<Mailbox>,
    pub reply_to: Option<Mailbox>,
    pub recipients: Vec<Recipient>,
    pub subject: String,
    pub text_body: Option<String>,
    pub html_body: Option<String>,
    pub attachments: Vec<Attachment>,
}

/// Operations exposed by a plugin through the authenticated admin API.
#[derive(Clone, Debug)]
pub enum ManagementOperation {
    List,
    Get { id: String },
    Create,
    Update { id: String },
    Delete { id: String },
}

#[derive(Clone, Debug)]
pub struct ManagementResult {
    pub status: u16,
    pub body: Option<Value>,
}

/// Host-managed, encrypted JSON storage scoped to the current plugin ID.
#[async_trait]
pub trait PluginStore: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<Value>, PluginError>;
    async fn put(&self, key: &str, value: &Value) -> Result<(), PluginError>;
    async fn delete(&self, key: &str) -> Result<bool, PluginError>;
}

#[derive(Clone)]
pub struct PluginContext {
    pub plugin_id: String,
    pub plugin_version: String,
    pub message_id: String,
    pub config: Value,
    pub held_state: Option<Value>,
    pub storage: Arc<dyn PluginStore>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HookDecision {
    Continue,
    Modify {
        draft: MailDraft,
    },
    Reject {
        code: String,
    },
    Hold {
        state: Value,
        #[serde(default)]
        resume_at: Option<String>,
    },
}

impl Default for HookDecision {
    fn default() -> Self {
        Self::Continue
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryResult {
    Accepted,
    TemporaryFailure { safe_code: String },
    PermanentFailure { safe_code: String },
    Unknown,
}

#[derive(Debug)]
pub struct PluginError {
    message: String,
}

impl PluginError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// A diagnostic for service logs. Never return this text directly to an API client.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for PluginError {}

#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Stable namespace. Keep this unchanged when upgrading the plugin.
    fn id(&self) -> &'static str;
    /// Code version used to interpret persisted config and held state.
    fn version(&self) -> &'static str;

    /// Configuration used until an administrator saves a replacement.
    fn default_config(&self) -> Value {
        Value::Object(Default::default())
    }

    /// A safe example shown when an administrator first edits this plugin's
    /// configuration. It is not applied until the administrator saves it.
    fn config_template(&self) -> Value {
        self.default_config()
    }

    /// Validate configuration before the host persists it. Plugins should
    /// deserialize into their own typed configuration and check its semantics.
    fn validate_config(&self, config: &Value) -> Result<(), PluginError> {
        if config.is_object() {
            Ok(())
        } else {
            Err(PluginError::new("configuration must be a JSON object"))
        }
    }

    /// Form a draft from plugin-specific request data before sender authorization.
    /// The host revalidates the result and then runs the normal mail hooks.
    async fn compose(
        &self,
        _draft: &MailDraft,
        _input: &Value,
        _context: &PluginContext,
    ) -> Result<HookDecision, PluginError> {
        Ok(HookDecision::Continue)
    }

    /// Plugin-owned admin resource. The host handles session and CSRF checks.
    async fn manage(
        &self,
        _resource: &str,
        _operation: ManagementOperation,
        _input: Option<Value>,
        _context: &PluginContext,
    ) -> Result<ManagementResult, PluginError> {
        Ok(ManagementResult { status: 404, body: None })
    }

    async fn before_submit(
        &self,
        _draft: &MailDraft,
        _context: &PluginContext,
    ) -> Result<HookDecision, PluginError> {
        Ok(HookDecision::Continue)
    }

    async fn before_send(
        &self,
        _draft: &MailDraft,
        _context: &PluginContext,
    ) -> Result<HookDecision, PluginError> {
        Ok(HookDecision::Continue)
    }

    async fn after_result(
        &self,
        _draft: &MailDraft,
        _result: &DeliveryResult,
        _context: &PluginContext,
    ) -> Result<(), PluginError> {
        Ok(())
    }
}
