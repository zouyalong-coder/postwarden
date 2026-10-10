//! Build-time plugin registry, persisted settings, and hook execution.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use postwarden_plugin_api::{
    DeliveryResult, HookDecision, MailDraft, Plugin, PluginContext, PluginError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;

use crate::{auth::AdminAuth, crypto::Crypto, database::Database, http::AppState};

/// Built-in plugins plus build-time registered project plugins.
#[derive(Clone)]
pub struct PluginManager {
    database: Database,
    crypto: Crypto,
    plugins: Arc<Vec<Arc<dyn Plugin>>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FlowOutcome {
    Continue {
        draft: MailDraft,
    },
    Reject {
        plugin_id: String,
        code: String,
    },
    Hold {
        plugin_id: String,
        draft: MailDraft,
        resume_at: Option<String>,
    },
}

#[derive(Debug)]
pub enum ManagerError {
    Database(sqlx::Error),
    Crypto,
    Serialization,
    Plugin {
        plugin_id: String,
        source: PluginError,
    },
    InvalidDecision {
        plugin_id: String,
        reason: &'static str,
    },
}

impl std::fmt::Display for ManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(_) => f.write_str("plugin database operation failed"),
            Self::Crypto => f.write_str("plugin data encryption operation failed"),
            Self::Serialization => f.write_str("plugin data serialization failed"),
            Self::Plugin { plugin_id, source } => write!(f, "plugin {plugin_id} failed: {source}"),
            Self::InvalidDecision { plugin_id, reason } => {
                write!(f, "plugin {plugin_id} returned invalid decision: {reason}")
            }
        }
    }
}

impl std::error::Error for ManagerError {}

#[derive(FromRow)]
struct ConfigRow {
    plugin_id: String,
    plugin_version: String,
    enabled: bool,
    execution_order: i32,
    config_ciphertext: Option<Vec<u8>>,
}

#[derive(FromRow)]
struct SqliteStateRow {
    plugin_version: String,
    state_ciphertext: Vec<u8>,
}

#[derive(FromRow)]
struct MysqlStateRow {
    plugin_version: String,
    state_ciphertext: Vec<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginInfo {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    pub execution_order: i32,
    pub has_config: bool,
}

impl PluginManager {
    /// Construct a registry and persist defaults for every linked plugin.
    /// Startup fails if a plugin version changed while it still owns held state.
    pub async fn new(database: Database, crypto: Crypto) -> Result<Self, ManagerError> {
        Self::with_plugins(database, crypto, vec![Arc::new(ExamplePlugin)]).await
    }

    /// Construct a registry with application plugins. Stable IDs must be unique.
    pub async fn with_plugins(
        database: Database,
        crypto: Crypto,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Result<Self, ManagerError> {
        let mut ids = std::collections::HashSet::new();
        for plugin in &plugins {
            if plugin.id().is_empty() || !ids.insert(plugin.id()) {
                return Err(ManagerError::InvalidDecision {
                    plugin_id: plugin.id().to_owned(),
                    reason: "plugin IDs must be nonempty and unique",
                });
            }
        }
        let manager = Self {
            database,
            crypto,
            plugins: Arc::new(plugins),
        };
        manager.initialize_registry().await?;
        Ok(manager)
    }

    async fn initialize_registry(&self) -> Result<(), ManagerError> {
        let existing = self.read_configs().await?;
        let mut next_order = existing
            .iter()
            .map(|row| row.execution_order)
            .max()
            .unwrap_or(-1)
            + 1;
        for plugin in self.plugins.iter() {
            let old = existing.iter().find(|row| row.plugin_id == plugin.id());
            if let Some(old) = old {
                if old.plugin_version != plugin.version() {
                    let pending = self.count_plugin_states(plugin.id()).await?;
                    if pending > 0 {
                        tracing::error!(plugin_id = plugin.id(), old_version = %old.plugin_version, new_version = plugin.version(), pending_states = pending, "plugin upgrade blocked by held message state");
                        return Err(ManagerError::InvalidDecision {
                            plugin_id: plugin.id().to_owned(),
                            reason: "held states must be resolved before upgrading plugin version",
                        });
                    }
                    self.update_version(plugin.id(), plugin.version()).await?;
                }
            } else {
                self.insert_default(plugin.id(), plugin.version(), next_order)
                    .await?;
                next_order += 1;
            }
        }
        Ok(())
    }

    /// Run all enabled plugins before the request is persisted. Holds are
    /// rejected here because held state requires an existing message record.
    pub async fn before_submit(
        &self,
        message_id: &str,
        mut draft: MailDraft,
    ) -> Result<FlowOutcome, ManagerError> {
        self.run_control_hooks(message_id, &mut draft, HookPhase::BeforeSubmit)
            .await
    }

    /// Run all enabled plugins immediately before SMTP. Holds are persisted
    /// against the message and plugin so the worker can resume the same state.
    pub async fn before_send(
        &self,
        message_id: &str,
        mut draft: MailDraft,
    ) -> Result<FlowOutcome, ManagerError> {
        self.run_control_hooks(message_id, &mut draft, HookPhase::BeforeSend)
            .await
    }

    pub async fn after_result(&self, message_id: &str, draft: &MailDraft, result: &DeliveryResult) {
        let configs = match self.ordered_configs().await {
            Ok(configs) => configs,
            Err(error) => {
                tracing::error!(error = %error, "could not load plugins after SMTP result");
                return;
            }
        };
        for (plugin, config) in configs {
            let state = match self.load_held_state(message_id, plugin.id()).await {
                Ok(state) => state,
                Err(error) => {
                    tracing::error!(plugin_id = plugin.id(), error = %error, "could not load plugin state after SMTP result");
                    continue;
                }
            };
            let context = self.context(plugin.as_ref(), message_id, config, state);
            if let Err(error) = plugin.after_result(draft, result, &context).await {
                // The already-observed SMTP result remains authoritative.
                tracing::error!(plugin_id = plugin.id(), error = %error, "plugin after_result hook failed");
            }
        }
    }

    pub async fn save_held_state(
        &self,
        message_id: &str,
        plugin_id: &str,
        state: &Value,
        resume_at: Option<&str>,
    ) -> Result<(), ManagerError> {
        let plugin = self
            .plugin(plugin_id)
            .ok_or_else(|| ManagerError::InvalidDecision {
                plugin_id: plugin_id.to_owned(),
                reason: "plugin is not registered",
            })?;
        let bytes = serde_json::to_vec(state).map_err(|_| ManagerError::Serialization)?;
        let encrypted = self
            .crypto
            .encrypt(&bytes)
            .map_err(|_| ManagerError::Crypto)?;
        let normalized_resume = parse_mysql_time(resume_at)?;
        match &self.database {
            Database::Sqlite(pool) => {
                let resume = normalized_resume
                    .map(|time| time.and_utc().to_rfc3339_opts(SecondsFormat::Micros, true));
                sqlx::query("INSERT INTO plugin_message_states(message_id,plugin_id,plugin_version,state_ciphertext,resume_at) VALUES(?,?,?,?,?) ON CONFLICT(message_id,plugin_id) DO UPDATE SET plugin_version=excluded.plugin_version,state_ciphertext=excluded.state_ciphertext,resume_at=excluded.resume_at")
                    .bind(message_id).bind(plugin_id).bind(plugin.version()).bind(encrypted).bind(resume)
                    .execute(pool).await.map_err(ManagerError::Database)?;
            }
            Database::Mysql(pool) => {
                let resume = parse_mysql_time(resume_at)?;
                sqlx::query("INSERT INTO plugin_message_states(message_id,plugin_id,plugin_version,state_ciphertext,resume_at) VALUES(?,?,?,?,?) ON DUPLICATE KEY UPDATE plugin_version=VALUES(plugin_version),state_ciphertext=VALUES(state_ciphertext),resume_at=VALUES(resume_at)")
                    .bind(message_id).bind(plugin_id).bind(plugin.version()).bind(encrypted).bind(resume)
                    .execute(pool).await.map_err(ManagerError::Database)?;
            }
        }
        Ok(())
    }

    pub async fn load_held_state(
        &self,
        message_id: &str,
        plugin_id: &str,
    ) -> Result<Option<Value>, ManagerError> {
        let row: Option<(String, Vec<u8>)> = match &self.database {
            Database::Sqlite(pool) => sqlx::query_as::<_, SqliteStateRow>("SELECT plugin_version,state_ciphertext FROM plugin_message_states WHERE message_id=? AND plugin_id=?")
                .bind(message_id).bind(plugin_id).fetch_optional(pool).await
                .map_err(ManagerError::Database)?.map(|r| (r.plugin_version,r.state_ciphertext)),
            Database::Mysql(pool) => sqlx::query_as::<_, MysqlStateRow>("SELECT plugin_version,state_ciphertext FROM plugin_message_states WHERE message_id=? AND plugin_id=?")
                .bind(message_id).bind(plugin_id).fetch_optional(pool).await
                .map_err(ManagerError::Database)?.map(|r| (r.plugin_version,r.state_ciphertext)),
        };
        let Some((version, ciphertext)) = row else {
            return Ok(None);
        };
        let plugin = self
            .plugin(plugin_id)
            .ok_or_else(|| ManagerError::InvalidDecision {
                plugin_id: plugin_id.to_owned(),
                reason: "held state belongs to an unregistered plugin",
            })?;
        if version != plugin.version() {
            return Err(ManagerError::InvalidDecision {
                plugin_id: plugin_id.to_owned(),
                reason: "held state plugin version does not match",
            });
        }
        let plaintext = self
            .crypto
            .decrypt(&ciphertext)
            .map_err(|_| ManagerError::Crypto)?;
        serde_json::from_slice(&plaintext)
            .map(Some)
            .map_err(|_| ManagerError::Serialization)
    }

    pub async fn clear_held_state(
        &self,
        message_id: &str,
        plugin_id: &str,
    ) -> Result<(), ManagerError> {
        match &self.database {
            Database::Sqlite(pool) => {
                sqlx::query("DELETE FROM plugin_message_states WHERE message_id=? AND plugin_id=?")
                    .bind(message_id)
                    .bind(plugin_id)
                    .execute(pool)
                    .await
                    .map_err(ManagerError::Database)?;
            }
            Database::Mysql(pool) => {
                sqlx::query("DELETE FROM plugin_message_states WHERE message_id=? AND plugin_id=?")
                    .bind(message_id)
                    .bind(plugin_id)
                    .execute(pool)
                    .await
                    .map_err(ManagerError::Database)?;
            }
        }
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<PluginInfo>, ManagerError> {
        let mut rows: Vec<_> = self
            .read_configs()
            .await?
            .into_iter()
            .filter_map(|row| self.plugin(&row.plugin_id).map(|plugin| (plugin, row)))
            .collect();
        rows.sort_by(|(plugin_a, row_a), (plugin_b, row_b)| {
            row_a
                .execution_order
                .cmp(&row_b.execution_order)
                .then_with(|| plugin_a.id().cmp(plugin_b.id()))
        });
        rows.into_iter()
            .map(|(plugin, row)| {
                Ok(PluginInfo {
                    id: plugin.id().to_owned(),
                    version: plugin.version().to_owned(),
                    enabled: row.enabled,
                    execution_order: row.execution_order,
                    has_config: row.config_ciphertext.is_some(),
                })
            })
            .collect()
    }

    async fn run_control_hooks(
        &self,
        message_id: &str,
        draft: &mut MailDraft,
        phase: HookPhase,
    ) -> Result<FlowOutcome, ManagerError> {
        for (plugin, config) in self.ordered_configs().await? {
            let previous_state = if phase == HookPhase::BeforeSend {
                self.load_held_state(message_id, plugin.id()).await?
            } else {
                None
            };
            let context = self.context(plugin.as_ref(), message_id, config, previous_state);
            let decision = match phase {
                HookPhase::BeforeSubmit => plugin.before_submit(draft, &context).await,
                HookPhase::BeforeSend => plugin.before_send(draft, &context).await,
            }
            .map_err(|source| ManagerError::Plugin {
                plugin_id: plugin.id().to_owned(),
                source,
            })?;
            match decision {
                HookDecision::Continue => {
                    if phase == HookPhase::BeforeSend && context.held_state.is_some() {
                        self.clear_held_state(message_id, plugin.id()).await?;
                    }
                }
                HookDecision::Modify { draft: modified } => {
                    *draft = modified;
                    if phase == HookPhase::BeforeSend && context.held_state.is_some() {
                        self.clear_held_state(message_id, plugin.id()).await?;
                    }
                }
                HookDecision::Reject { code } => {
                    if !safe_code(&code) {
                        return Err(ManagerError::InvalidDecision {
                            plugin_id: plugin.id().to_owned(),
                            reason: "reject code is invalid",
                        });
                    }
                    return Ok(FlowOutcome::Reject {
                        plugin_id: plugin.id().to_owned(),
                        code,
                    });
                }
                HookDecision::Hold { state, resume_at } => {
                    if phase == HookPhase::BeforeSubmit {
                        return Err(ManagerError::InvalidDecision {
                            plugin_id: plugin.id().to_owned(),
                            reason: "before_submit cannot hold a message",
                        });
                    }
                    self.save_held_state(message_id, plugin.id(), &state, resume_at.as_deref())
                        .await?;
                    return Ok(FlowOutcome::Hold {
                        plugin_id: plugin.id().to_owned(),
                        draft: draft.clone(),
                        resume_at,
                    });
                }
            }
        }
        Ok(FlowOutcome::Continue {
            draft: draft.clone(),
        })
    }

    fn context(
        &self,
        plugin: &dyn Plugin,
        message_id: &str,
        config: Value,
        held_state: Option<Value>,
    ) -> PluginContext {
        PluginContext {
            plugin_id: plugin.id().to_owned(),
            plugin_version: plugin.version().to_owned(),
            message_id: message_id.to_owned(),
            config,
            held_state,
        }
    }

    fn plugin(&self, id: &str) -> Option<Arc<dyn Plugin>> {
        self.plugins.iter().find(|p| p.id() == id).cloned()
    }

    async fn ordered_configs(&self) -> Result<Vec<(Arc<dyn Plugin>, Value)>, ManagerError> {
        let rows = self.read_configs().await?;
        let mut plugins: Vec<_> = rows
            .into_iter()
            .filter(|row| row.enabled)
            .filter_map(|row| self.plugin(&row.plugin_id).map(|p| (p, row)))
            .collect();
        plugins.sort_by(|(plugin_a, row_a), (plugin_b, row_b)| {
            row_a
                .execution_order
                .cmp(&row_b.execution_order)
                .then_with(|| plugin_a.id().cmp(plugin_b.id()))
        });
        plugins
            .into_iter()
            .map(|(plugin, row)| {
                let config = match row.config_ciphertext {
                    None => json!({}),
                    Some(encrypted) => {
                        let plain = self
                            .crypto
                            .decrypt(&encrypted)
                            .map_err(|_| ManagerError::Crypto)?;
                        serde_json::from_slice(&plain).map_err(|_| ManagerError::Serialization)?
                    }
                };
                Ok((plugin, config))
            })
            .collect()
    }

    async fn read_configs(&self) -> Result<Vec<ConfigRow>, ManagerError> {
        match &self.database {
            Database::Sqlite(pool) => sqlx::query_as::<_, ConfigRow>("SELECT plugin_id,plugin_version,enabled,execution_order,config_ciphertext FROM plugin_configs").fetch_all(pool).await.map_err(ManagerError::Database),
            Database::Mysql(pool) => sqlx::query_as::<_, ConfigRow>("SELECT plugin_id,plugin_version,enabled,execution_order,config_ciphertext FROM plugin_configs").fetch_all(pool).await.map_err(ManagerError::Database),
        }
    }

    async fn config_row(&self, id: &str) -> Result<Option<ConfigRow>, ManagerError> {
        match &self.database {
            Database::Sqlite(pool) => sqlx::query_as::<_, ConfigRow>("SELECT plugin_id,plugin_version,enabled,execution_order,config_ciphertext FROM plugin_configs WHERE plugin_id=?").bind(id).fetch_optional(pool).await.map_err(ManagerError::Database),
            Database::Mysql(pool) => sqlx::query_as::<_, ConfigRow>("SELECT plugin_id,plugin_version,enabled,execution_order,config_ciphertext FROM plugin_configs WHERE plugin_id=?").bind(id).fetch_optional(pool).await.map_err(ManagerError::Database),
        }
    }

    async fn count_plugin_states(&self, id: &str) -> Result<i64, ManagerError> {
        match &self.database {
            Database::Sqlite(pool) => {
                sqlx::query_scalar("SELECT COUNT(*) FROM plugin_message_states s JOIN messages m ON m.id=s.message_id WHERE s.plugin_id=? AND m.status='held'")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .map_err(ManagerError::Database)
            }
            Database::Mysql(pool) => {
                sqlx::query_scalar("SELECT COUNT(*) FROM plugin_message_states s JOIN messages m ON m.id=s.message_id WHERE s.plugin_id=? AND m.status='held'")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .map_err(ManagerError::Database)
            }
        }
    }

    async fn insert_default(
        &self,
        id: &str,
        version: &str,
        order: i32,
    ) -> Result<(), ManagerError> {
        match &self.database {
            Database::Sqlite(pool) => {
                sqlx::query("INSERT INTO plugin_configs(plugin_id,plugin_version,enabled,execution_order,config_ciphertext,updated_at) VALUES(?,?,1,?,NULL,?)").bind(id).bind(version).bind(order).bind(Utc::now().to_rfc3339()).execute(pool).await.map_err(ManagerError::Database)?;
            }
            Database::Mysql(pool) => {
                sqlx::query("INSERT INTO plugin_configs(plugin_id,plugin_version,enabled,execution_order,config_ciphertext,updated_at) VALUES(?,?,TRUE,?,NULL,?)").bind(id).bind(version).bind(order).bind(Utc::now().naive_utc()).execute(pool).await.map_err(ManagerError::Database)?;
            }
        }
        Ok(())
    }

    async fn update_version(&self, id: &str, version: &str) -> Result<(), ManagerError> {
        match &self.database {
            Database::Sqlite(pool) => {
                sqlx::query(
                    "UPDATE plugin_configs SET plugin_version=?,updated_at=? WHERE plugin_id=?",
                )
                .bind(version)
                .bind(Utc::now().to_rfc3339())
                .bind(id)
                .execute(pool)
                .await
                .map_err(ManagerError::Database)?;
            }
            Database::Mysql(pool) => {
                sqlx::query(
                    "UPDATE plugin_configs SET plugin_version=?,updated_at=? WHERE plugin_id=?",
                )
                .bind(version)
                .bind(Utc::now().naive_utc())
                .bind(id)
                .execute(pool)
                .await
                .map_err(ManagerError::Database)?;
            }
        }
        Ok(())
    }

    async fn update_config(
        &self,
        id: &str,
        enabled: bool,
        order: i32,
        config: Option<Value>,
    ) -> Result<(), ManagerError> {
        let ciphertext = match config {
            Some(config) => Some(
                self.crypto
                    .encrypt(&serde_json::to_vec(&config).map_err(|_| ManagerError::Serialization)?)
                    .map_err(|_| ManagerError::Crypto)?,
            ),
            None => self
                .config_row(id)
                .await?
                .and_then(|row| row.config_ciphertext),
        };
        match &self.database {
            Database::Sqlite(pool) => {
                sqlx::query("UPDATE plugin_configs SET enabled=?,execution_order=?,config_ciphertext=?,updated_at=? WHERE plugin_id=?").bind(enabled).bind(order).bind(ciphertext).bind(Utc::now().to_rfc3339()).bind(id).execute(pool).await.map_err(ManagerError::Database)?;
            }
            Database::Mysql(pool) => {
                sqlx::query("UPDATE plugin_configs SET enabled=?,execution_order=?,config_ciphertext=?,updated_at=? WHERE plugin_id=?").bind(enabled).bind(order).bind(ciphertext).bind(Utc::now().naive_utc()).bind(id).execute(pool).await.map_err(ManagerError::Database)?;
            }
        }
        Ok(())
    }

    async fn update_plugin(
        &self,
        id: &str,
        update: PluginUpdate,
    ) -> Result<PluginInfo, ManagerError> {
        let Some(plugin) = self.plugin(id) else {
            return Err(ManagerError::InvalidDecision {
                plugin_id: id.to_owned(),
                reason: "plugin is not registered",
            });
        };
        let row = self
            .config_row(id)
            .await?
            .ok_or_else(|| ManagerError::InvalidDecision {
                plugin_id: id.to_owned(),
                reason: "plugin configuration is missing",
            })?;
        if update.enabled == Some(false) && row.enabled {
            let pending = self.count_plugin_states(id).await?;
            if pending > 0 {
                return Err(ManagerError::InvalidDecision {
                    plugin_id: id.to_owned(),
                    reason: "held states must be resolved before disabling plugin",
                });
            }
        }
        self.update_config(
            id,
            update.enabled.unwrap_or(row.enabled),
            update.execution_order.unwrap_or(row.execution_order),
            update.config,
        )
        .await?;
        self.list()
            .await?
            .into_iter()
            .find(|item| item.id == plugin.id())
            .ok_or_else(|| ManagerError::InvalidDecision {
                plugin_id: id.to_owned(),
                reason: "plugin configuration disappeared",
            })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookPhase {
    BeforeSubmit,
    BeforeSend,
}

fn safe_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn parse_mysql_time(value: Option<&str>) -> Result<Option<NaiveDateTime>, ManagerError> {
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map(|time| time.with_timezone(&Utc).naive_utc())
                .map_err(|_| ManagerError::InvalidDecision {
                    plugin_id: "unknown".to_owned(),
                    reason: "resume_at must be an RFC 3339 timestamp",
                })
        })
        .transpose()
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExampleConfig {
    subject_prefix: Option<String>,
    reject_subject_contains: Option<String>,
    hold_subject_contains: Option<String>,
}

struct ExamplePlugin;

#[postwarden_plugin_api::async_trait]
impl Plugin for ExamplePlugin {
    fn id(&self) -> &'static str {
        "example-policy"
    }
    fn version(&self) -> &'static str {
        "1"
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

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/plugins", get(list_plugins))
        .route("/plugins/{id}", put(update_plugin))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginUpdate {
    enabled: Option<bool>,
    execution_order: Option<i32>,
    config: Option<Value>,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

async fn list_plugins(
    _: AdminAuth,
    State(state): State<AppState>,
) -> Result<Json<Vec<PluginInfo>>, PluginApiError> {
    state
        .plugins
        .list()
        .await
        .map(Json)
        .map_err(PluginApiError::from)
}

async fn update_plugin(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<PluginUpdate>,
) -> Result<Json<PluginInfo>, PluginApiError> {
    if input.execution_order.is_some_and(|order| order < 0) {
        return Err(PluginApiError::invalid());
    }
    state
        .plugins
        .update_plugin(&id, input)
        .await
        .map(Json)
        .map_err(PluginApiError::from)
}

struct PluginApiError(StatusCode, &'static str, &'static str);

impl PluginApiError {
    fn invalid() -> Self {
        Self(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Plugin settings are invalid",
        )
    }
}

impl From<ManagerError> for PluginApiError {
    fn from(error: ManagerError) -> Self {
        tracing::error!(error = %error, "plugin management request failed");
        match error {
            ManagerError::InvalidDecision { reason, .. }
                if reason == "plugin is not registered" =>
            {
                Self(StatusCode::NOT_FOUND, "not_found", "Plugin not found")
            }
            ManagerError::InvalidDecision { reason, .. } if reason.contains("held states") => Self(
                StatusCode::CONFLICT,
                "plugin_has_held_messages",
                "Resolve held messages before changing this plugin",
            ),
            ManagerError::InvalidDecision { reason, .. } if reason.contains("invalid") => Self(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Plugin settings are invalid",
            ),
            _ => Self(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Request could not be completed",
            ),
        }
    }
}

impl axum::response::IntoResponse for PluginApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.0,
            Json(ErrorBody {
                code: self.1,
                message: self.2,
            }),
        )
            .into_response()
    }
}
