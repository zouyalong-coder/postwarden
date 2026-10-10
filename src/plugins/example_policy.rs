//! Example message policy plugin.

use postwarden_plugin_api::{HookDecision, MailDraft, Plugin, PluginContext, PluginError};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExampleConfig {
    subject_prefix: Option<String>,
    reject_subject_contains: Option<String>,
    hold_subject_contains: Option<String>,
}

pub(super) struct ExamplePlugin;

#[postwarden_plugin_api::async_trait]
impl Plugin for ExamplePlugin {
    fn id(&self) -> &'static str {
        "example-policy"
    }
    fn version(&self) -> &'static str {
        "1"
    }

    fn config_template(&self) -> Value {
        json!({
            "subject_prefix": "[PostWarden] ",
            "reject_subject_contains": "blocked",
            "hold_subject_contains": "review"
        })
    }

    fn validate_config(&self, config: &Value) -> Result<(), PluginError> {
        serde_json::from_value::<ExampleConfig>(config.clone())
            .map(|_| ())
            .map_err(|error| PluginError::new(format!("invalid example-policy config: {error}")))
    }

    async fn before_submit(
        &self,
        draft: &MailDraft,
        context: &PluginContext,
    ) -> Result<HookDecision, PluginError> {
        let config: ExampleConfig = serde_json::from_value(context.config.clone())
            .map_err(|error| PluginError::new(format!("invalid example-policy config: {error}")))?;
        if config
            .reject_subject_contains
            .as_ref()
            .is_some_and(|needle| draft.subject.contains(needle))
        {
            return Ok(HookDecision::Reject {
                code: "example_policy_rejected".to_owned(),
            });
        }
        if let Some(prefix) = config.subject_prefix.filter(|prefix| !prefix.is_empty()) {
            let mut changed = draft.clone();
            if !changed.subject.starts_with(&prefix) {
                changed.subject = format!("{prefix}{}", changed.subject);
                return Ok(HookDecision::Modify { draft: changed });
            }
        }
        Ok(HookDecision::Continue)
    }

    async fn before_send(
        &self,
        draft: &MailDraft,
        context: &PluginContext,
    ) -> Result<HookDecision, PluginError> {
        let config: ExampleConfig = serde_json::from_value(context.config.clone())
            .map_err(|error| PluginError::new(format!("invalid example-policy config: {error}")))?;
        if config
            .hold_subject_contains
            .as_ref()
            .is_some_and(|needle| draft.subject.contains(needle))
            && context.held_state.is_none()
        {
            return Ok(HookDecision::Hold {
                state: json!({"reason":"example policy review"}),
                resume_at: None,
            });
        }
        if context.held_state.is_some() {
            return Ok(HookDecision::Continue);
        }
        Ok(HookDecision::Continue)
    }
}
