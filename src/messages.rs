use std::str::FromStr;

use axum::{
    body::to_bytes,
    extract::{Path, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, NaiveDateTime, Utc};
use postwarden_plugin_api::{
    Attachment as PluginAttachment, DeliveryResult, MailDraft, Mailbox, Recipient,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{database::Database, http::AppState, plugin_runtime::FlowOutcome, smtp_transport};

const MAX_REQUEST: usize = 24 * 1024 * 1024;
const MAX_ATTACHMENT: usize = 8 * 1024 * 1024;
const MAX_ATTACHMENTS: usize = 10;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/messages", post(async_send))
        .route("/messages/send", post(sync_send))
        .route("/messages/{id}", get(get_message))
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct SendRequest {
    #[serde(default)]
    sender_identity_id: String,
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    bcc: Vec<String>,
    #[serde(default)]
    subject: String,
    text: Option<String>,
    html: Option<String>,
    #[serde(default)]
    attachments: Vec<JsonAttachment>,
    #[serde(default)]
    plugin_id: Option<String>,
    #[serde(default)]
    plugin_input: OptionalPluginInput,
}
#[derive(Debug, Clone)]
struct OptionalPluginInput {
    present: bool,
    value: Value,
}
impl Default for OptionalPluginInput {
    fn default() -> Self {
        Self {
            present: false,
            value: Value::Null,
        }
    }
}
impl<'de> Deserialize<'de> for OptionalPluginInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(|value| Self {
            present: true,
            value,
        })
    }
}
impl Serialize for OptionalPluginInput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.value.serialize(serializer)
    }
}
impl SendRequest {
    fn plugin_invocation(&self) -> Result<Option<PluginInvocation>, ApiError> {
        match (&self.plugin_id, self.plugin_input.present) {
            (Some(plugin_id), true) => Ok(Some(PluginInvocation {
                plugin_id: plugin_id.clone(),
                input: self.plugin_input.value.clone(),
            })),
            (Some(_), false) | (None, true) => Err(bad()),
            (None, false) => Ok(None),
        }
    }
}
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct PluginInvocation {
    plugin_id: String,
    input: Value,
}
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct JsonAttachment {
    filename: String,
    content_type: String,
    data_base64: String,
}
#[derive(Debug, Deserialize, Serialize, Clone)]
struct MessagePayload {
    draft: MailDraft,
    #[serde(default)]
    before_send_complete: bool,
}
#[derive(Debug, Deserialize, Serialize)]
struct RecipientAudit {
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
}

#[derive(Serialize)]
struct SendResponse {
    message_id: String,
    status: String,
    status_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    smtp_accepted_at: Option<String>,
}
#[derive(Serialize)]
struct MessageView {
    message_id: String,
    mode: String,
    sender_identity_id: String,
    status: String,
    created_at: String,
    finished_at: Option<String>,
    attempt_count: i64,
    last_error_code: Option<String>,
    recipients: Option<Vec<String>>,
}
#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_url: Option<String>,
}
#[derive(Debug)]
struct ApiError(StatusCode, &'static str, &'static str, Option<String>);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message, id) = (self.0, self.1, self.2, self.3);
        (
            status,
            Json(ErrorBody {
                code,
                message,
                status_url: id.as_ref().map(|x| format!("/api/v1/messages/{x}")),
                message_id: id,
            }),
        )
            .into_response()
    }
}
fn bad() -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Message request is invalid",
        None,
    )
}
fn internal() -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Request could not be completed",
        None,
    )
}
fn mapdb<T>(r: Result<T, sqlx::Error>) -> Result<T, ApiError> {
    r.map_err(|e| {
        tracing::error!(error=%e,"message database operation failed");
        internal()
    })
}

#[derive(FromRow)]
struct IdentitySqlite {
    id: String,
    display_name: String,
    email_address: String,
    envelope_from: String,
    version: i64,
    host: String,
    port: i64,
    tls_mode: String,
    auth_type: String,
    username: Option<String>,
    credential_ciphertext: Option<Vec<u8>>,
}
#[derive(FromRow)]
struct IdentityMysql {
    id: String,
    display_name: String,
    email_address: String,
    envelope_from: String,
    version: i64,
    host: String,
    port: u16,
    tls_mode: String,
    auth_type: String,
    username: Option<String>,
    credential_ciphertext: Option<Vec<u8>>,
}
struct Identity {
    id: String,
    display_name: String,
    email_address: String,
    envelope_from: String,
    version: i64,
    host: String,
    port: u16,
    tls_mode: String,
    auth_type: String,
    username: Option<String>,
    credential: Option<String>,
}
impl Identity {
    fn from_sqlite(r: IdentitySqlite, crypto: &crate::crypto::Crypto) -> Result<Self, ApiError> {
        let credential = r
            .credential_ciphertext
            .map(|v| crypto.decrypt(&v).map_err(|_| internal()))
            .transpose()?
            .map(|v| String::from_utf8(v).map_err(|_| internal()))
            .transpose()?;
        Ok(Self {
            id: r.id,
            display_name: r.display_name,
            email_address: r.email_address,
            envelope_from: r.envelope_from,
            version: r.version,
            host: r.host,
            port: r.port as u16,
            tls_mode: r.tls_mode,
            auth_type: r.auth_type,
            username: r.username,
            credential,
        })
    }
    fn from_mysql(r: IdentityMysql, crypto: &crate::crypto::Crypto) -> Result<Self, ApiError> {
        let credential = r
            .credential_ciphertext
            .map(|v| crypto.decrypt(&v).map_err(|_| internal()))
            .transpose()?
            .map(|v| String::from_utf8(v).map_err(|_| internal()))
            .transpose()?;
        Ok(Self {
            id: r.id,
            display_name: r.display_name,
            email_address: r.email_address,
            envelope_from: r.envelope_from,
            version: r.version,
            host: r.host,
            port: r.port,
            tls_mode: r.tls_mode,
            auth_type: r.auth_type,
            username: r.username,
            credential,
        })
    }
}

async fn load_identity(
    db: &Database,
    crypto: &crate::crypto::Crypto,
    id: &str,
) -> Result<Option<Identity>, ApiError> {
    match db {
        Database::Sqlite(p)=>mapdb(sqlx::query_as::<_,IdentitySqlite>("SELECT i.id,i.display_name,i.email_address,i.envelope_from,i.version,a.host,a.port,a.tls_mode,a.auth_type,a.username,a.credential_ciphertext FROM sender_identities i JOIN smtp_accounts a ON a.id=i.smtp_account_id WHERE i.id=? AND i.enabled=1 AND i.deleted_at IS NULL AND a.enabled=1 AND a.deleted_at IS NULL").bind(id).fetch_optional(p).await)?.map(|r|Identity::from_sqlite(r,crypto)).transpose(),
        Database::Mysql(p)=>mapdb(sqlx::query_as::<_,IdentityMysql>("SELECT i.id,i.display_name,i.email_address,i.envelope_from,i.version,a.host,a.port,a.tls_mode,a.auth_type,a.username,a.credential_ciphertext FROM sender_identities i JOIN smtp_accounts a ON a.id=i.smtp_account_id WHERE i.id=? AND i.enabled=TRUE AND i.deleted_at IS NULL AND a.enabled=TRUE AND a.deleted_at IS NULL").bind(id).fetch_optional(p).await)?.map(|r|Identity::from_mysql(r,crypto)).transpose(),
    }
}

fn raw_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}
async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<crate::tokens::TokenPrincipal, ApiError> {
    let raw = raw_token(headers).ok_or(ApiError(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "A valid API token is required",
        None,
    ))?;
    crate::tokens::authenticate(&state.database, raw)
        .await
        .map_err(|e| {
            tracing::error!(error=%e,"token authentication failed");
            internal()
        })?
        .ok_or(ApiError(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid API token is required",
            None,
        ))
}

async fn async_send(
    State(state): State<AppState>,
    request: Request,
) -> Result<(StatusCode, Json<SendResponse>), ApiError> {
    submit_request(state, request, "async").await
}
async fn sync_send(State(state): State<AppState>, request: Request) -> Result<Response, ApiError> {
    submit_request(state, request, "sync")
        .await
        .map(|(_, json)| (StatusCode::OK, json).into_response())
}

async fn submit_request(
    state: AppState,
    request: Request,
    mode: &str,
) -> Result<(StatusCode, Json<SendResponse>), ApiError> {
    let (parts, body) = request.into_parts();
    let principal = authenticate(&state, &parts.headers).await?;
    let key = parts
        .headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty() && v.len() <= 255)
        .ok_or_else(bad)?
        .to_owned();
    let raw = to_bytes(body, MAX_REQUEST).await.map_err(|_| {
        ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "Message payload is too large",
            None,
        )
    })?;
    let (input, attachments) = parse_body(&parts.headers, &raw)?;
    let invocation = input.plugin_invocation()?;
    let source = invocation
        .as_ref()
        .map(|_| serde_json::to_string(&(&input, &attachments)))
        .transpose()
        .map_err(|_| internal())?;
    if let Some(source) = source.as_deref() {
        let digest = request_digest(mode, &MailDraft::default(), Some(source))?;
        if let Some(existing) = find_idempotent(&state.database, &principal.id, &key).await? {
            return replay_existing(&state, &principal.id, mode, &digest, existing).await;
        }
    }
    let id = format!("msg_{}", Uuid::new_v4().simple());
    let mut draft = make_draft(&input, &attachments, None)?;
    if let Some(plugin) = &invocation {
        validate_plugin_input(&input, plugin, &attachments)?;
        match state
            .plugins
            .compose(&plugin.plugin_id, &id, &draft, &plugin.input)
            .await
        {
            Ok(FlowOutcome::Continue { draft: modified }) => draft = modified,
            Ok(FlowOutcome::Reject { code, .. }) => {
                let (status, safe_code) = match code.as_str() {
                    "template_not_found" => (StatusCode::NOT_FOUND, "template_not_found"),
                    "template_scenario_required" => {
                        (StatusCode::BAD_REQUEST, "template_scenario_required")
                    }
                    "template_variables_invalid" => (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "template_variables_invalid",
                    ),
                    _ => (StatusCode::UNPROCESSABLE_ENTITY, "template_rejected"),
                };
                return Err(ApiError(
                    status,
                    safe_code,
                    "Template could not form a message",
                    None,
                ));
            }
            Ok(FlowOutcome::Hold { .. }) => return Err(internal()),
            Err(crate::plugin_runtime::ManagerError::InvalidDecision {
                reason: "plugin is disabled",
                ..
            }) => {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "plugin_disabled",
                    "Template plugin is disabled",
                    None,
                ));
            }
            Err(crate::plugin_runtime::ManagerError::InvalidDecision {
                reason: "plugin is not registered",
                ..
            }) => {
                return Err(ApiError(
                    StatusCode::NOT_FOUND,
                    "plugin_not_found",
                    "Template plugin is not registered",
                    None,
                ));
            }
            Err(error) => {
                tracing::error!(error = %error, "template compose failed");
                return Err(ApiError(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "template_unavailable",
                    "Template plugin is unavailable or failed",
                    None,
                ));
            }
        }
    } else {
        validate_request(&input, &attachments)?;
    }
    let allowed = crate::tokens::authorize_identity(
        &state.database,
        &principal.id,
        &draft.sender_identity_id,
    )
    .await
    .map_err(|e| {
        tracing::error!(error=%e,"sender authorization failed");
        internal()
    })?;
    if !allowed {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "sender_not_authorized",
            "Token is not authorized for this sender identity",
            None,
        ));
    }
    let Some(identity) =
        load_identity(&state.database, &state.crypto, &draft.sender_identity_id).await?
    else {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "sender_unavailable",
            "Sender identity is unavailable",
            None,
        ));
    };
    draft.from = Some(Mailbox {
        name: Some(identity.display_name.clone()),
        address: identity.email_address.clone(),
    });
    validate_draft(&draft, &identity)?;
    let digest = request_digest(mode, &draft, source.as_deref())?;
    let recipient_data = serde_json::to_vec(&recipient_audit(&draft)).map_err(|_| internal())?;
    let recipient_cipher = state
        .crypto
        .encrypt(&recipient_data)
        .map_err(|_| internal())?;
    let now = Utc::now();
    let created = now.to_rfc3339();
    let expires = if mode == "async" {
        Some((now + chrono::Duration::days(state.retention.pending_days)).to_rfc3339())
    } else {
        None
    };

    if let Some(existing) = find_idempotent(&state.database, &principal.id, &key).await? {
        return replay_existing(&state, &principal.id, mode, &digest, existing).await;
    }

    let payload = if mode == "async" {
        Some(
            state
                .crypto
                .encrypt(
                    &serde_json::to_vec(&MessagePayload {
                        draft: draft.clone(),
                        before_send_complete: false,
                    })
                    .map_err(|_| internal())?,
                )
                .map_err(|_| internal())?,
        )
    } else {
        None
    };
    let inserted = insert_message(
        &state,
        &principal.id,
        &id,
        &draft.sender_identity_id,
        identity.version,
        mode,
        &key,
        &digest,
        &recipient_cipher,
        &created,
        expires.as_deref(),
        "processing",
        payload.as_deref(),
    )
    .await?;
    if !inserted {
        let existing = find_idempotent(&state.database, &principal.id, &key)
            .await?
            .ok_or_else(internal)?;
        if existing.0 != digest || existing.1 != mode {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "Idempotency-Key was used with a different request",
                Some(existing.2),
            ));
        }
        if mode == "sync" && existing.3 != "smtp_accepted" {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "message_processing",
                "The original request already owns this key",
                Some(existing.2),
            ));
        }
        return Ok((
            if mode == "async" {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            Json(SendResponse {
                message_id: existing.2.clone(),
                status: existing.3,
                status_url: format!("/api/v1/messages/{}", existing.2),
                smtp_accepted_at: None,
            }),
        ));
    }
    insert_event(&state.database, &id, "created", None, None).await?;
    let (initial_status, hook_error) = match state.plugins.before_submit(&id, draft.clone()).await {
        Ok(FlowOutcome::Continue { draft: d }) => {
            draft = d;
            ("processing", None)
        }
        Ok(FlowOutcome::Reject { code, .. }) => ("rejected", Some(safe_plugin_code(&code))),
        Ok(FlowOutcome::Hold { .. }) => ("rejected", Some("requires_async".to_owned())),
        Err(e) => {
            tracing::error!(error=%e,"before_submit plugin failed");
            ("failed", Some("plugin_error".to_owned()))
        }
    };
    if initial_status == "processing" {
        if validate_draft(&draft, &identity).is_err() {
            finish(&state, &id, "failed", "plugin_invalid_draft").await?;
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "plugin_error",
                "Message processing failed",
                Some(id),
            ));
        }
    }
    if hook_error.as_deref() == Some("requires_async") {
        finish(&state, &id, "rejected", "requires_async").await?;
        return Err(ApiError(
            StatusCode::CONFLICT,
            "requires_async",
            "Message requires asynchronous processing",
            Some(id),
        ));
    }
    if initial_status == "rejected" || initial_status == "failed" {
        finish(
            &state,
            &id,
            initial_status,
            hook_error.as_deref().unwrap_or("plugin_rejected"),
        )
        .await?;
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            if initial_status == "rejected" {
                "message_rejected"
            } else {
                "plugin_error"
            },
            "Message was not sent",
            Some(id),
        ));
    }
    if mode == "async" {
        let payload = MessagePayload {
            draft: draft.clone(),
            before_send_complete: false,
        };
        persist_payload(&state, &id, &payload).await?;
        set_queued(&state, &id).await?;
        insert_event(&state.database, &id, "queued", None, None).await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(SendResponse {
                message_id: id.clone(),
                status: "queued".into(),
                status_url: format!("/api/v1/messages/{id}"),
                smtp_accepted_at: None,
            }),
        ));
    }
    if mode == "sync" {
        let processed = match process_message(&state, &id, false, Some(draft)).await {
            Ok(value) => value,
            Err(error) => {
                let _ = mark_inflight_unknown(&state, &id).await;
                return Err(ApiError(error.0, error.1, error.2, Some(id)));
            }
        };
        if let Some((code, message, detail)) = processed {
            return Err(ApiError(code, message, detail, Some(id)));
        }
        let current = fetch_status(&state, &id, None, true)
            .await?
            .ok_or_else(internal)?;
        if current.status != "smtp_accepted" {
            return Err(ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "outcome_unknown",
                "SMTP delivery result could not be confirmed",
                Some(id),
            ));
        }
        return Ok((
            StatusCode::OK,
            Json(SendResponse {
                message_id: id.clone(),
                status: current.status,
                status_url: format!("/api/v1/messages/{id}"),
                smtp_accepted_at: current.finished_at,
            }),
        ));
    }
    Err(internal())
}

fn parse_body(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(SendRequest, Vec<PluginAttachment>), ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");
    if content_type.starts_with("application/json") {
        let req: SendRequest = serde_json::from_slice(body).map_err(|_| bad())?;
        let files = req
            .attachments
            .iter()
            .map(decode_attachment)
            .collect::<Result<Vec<_>, _>>()?;
        return Ok((req, files));
    }
    if content_type.starts_with("multipart/form-data") {
        return parse_multipart(content_type, body);
    }
    Err(ApiError(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
        "Use application/json or multipart/form-data",
        None,
    ))
}
fn decode_attachment(a: &JsonAttachment) -> Result<PluginAttachment, ApiError> {
    let data = STANDARD.decode(&a.data_base64).map_err(|_| bad())?;
    if data.len() > MAX_ATTACHMENT {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "Attachment is too large",
            None,
        ));
    }
    Ok(PluginAttachment {
        filename: a.filename.clone(),
        content_type: a.content_type.clone(),
        data_base64: STANDARD.encode(data),
        inline_content_id: None,
    })
}
fn parse_multipart(
    content_type: &str,
    body: &[u8],
) -> Result<(SendRequest, Vec<PluginAttachment>), ApiError> {
    let boundary = content_type
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("boundary="))
        .map(|b| b.trim_matches('"'))
        .filter(|b| {
            !b.is_empty()
                && b.len() <= 70
                && b.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"'()+_,-./:=?".contains(&c))
        })
        .ok_or_else(bad)?;
    let delimiter = format!("--{boundary}");
    let mut message = None;
    let mut files = Vec::new();
    let marker = delimiter.as_bytes();
    let mut cursor = 0;
    while let Some(boundary_at) = find_multipart_boundary(body, marker, cursor) {
        let after = boundary_at + marker.len();
        if body.get(after..after + 2) == Some(b"--") {
            break;
        }
        let start = if body.get(after..after + 2) == Some(b"\r\n") {
            after + 2
        } else {
            return Err(bad());
        };
        let Some(next_at) = find_multipart_boundary(body, marker, start) else {
            break;
        };
        let mut part = &body[start..next_at];
        if part.ends_with(b"\r\n") {
            part = &part[..part.len() - 2]
        }
        cursor = next_at;
        let Some(header_end) = find_subslice(part, b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&part[..header_end]);
        let data = &part[header_end + 4..];
        let disposition = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-disposition:"))
            .unwrap_or("");
        let name = quoted_param(disposition, "name").unwrap_or_default();
        if name == "message" {
            message = Some(serde_json::from_slice::<SendRequest>(data).map_err(|_| bad())?);
        } else if !name.is_empty() {
            if files.len() >= MAX_ATTACHMENTS {
                return Err(ApiError(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "payload_too_large",
                    "Too many attachments",
                    None,
                ));
            };
            let filename = quoted_param(disposition, "filename").unwrap_or(name);
            let ct = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .starts_with("content-type:")
                        .then(|| l.split_once(':').map(|(_, v)| v.trim().to_owned()))
                        .flatten()
                })
                .unwrap_or_else(|| "application/octet-stream".to_owned());
            if data.len() > MAX_ATTACHMENT {
                return Err(ApiError(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "payload_too_large",
                    "Attachment is too large",
                    None,
                ));
            };
            files.push(PluginAttachment {
                filename,
                content_type: ct,
                data_base64: STANDARD.encode(data),
                inline_content_id: None,
            });
        }
    }
    let req = message.ok_or_else(bad)?;
    if !req.attachments.is_empty() {
        return Err(bad());
    }
    Ok((req, files))
}
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
fn find_multipart_boundary(body: &[u8], marker: &[u8], from: usize) -> Option<usize> {
    let mut cursor = from;
    while cursor < body.len() {
        let rel = find_subslice(&body[cursor..], marker)?;
        let at = cursor + rel;
        let end = at + marker.len();
        let line_start = at == 0 || body.get(at.saturating_sub(2)..at) == Some(b"\r\n");
        let line_end =
            body.get(end..end + 2) == Some(b"\r\n") || body.get(end..end + 2) == Some(b"--");
        if line_start && line_end {
            return Some(at);
        }
        cursor = at + 1;
    }
    None
}
fn quoted_param(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let p = line.find(&needle)? + needle.len();
    let e = line[p..].find('"')?;
    Some(line[p..p + e].to_owned())
}

fn validate_request(r: &SendRequest, files: &[PluginAttachment]) -> Result<(), ApiError> {
    let total: usize = files
        .iter()
        .map(|f| {
            STANDARD
                .decode(&f.data_base64)
                .map(|b| b.len())
                .unwrap_or(usize::MAX)
        })
        .sum();
    if r.sender_identity_id.is_empty()
        || r.to.is_empty()
        || r.subject.is_empty()
        || r.subject.len() > 998
        || r.subject.contains(['\r', '\n'])
        || r.to.len() + r.cc.len() + r.bcc.len() > 100
        || r.text.as_ref().is_some_and(|v| v.len() > MAX_REQUEST / 2)
        || r.html.as_ref().is_some_and(|v| v.len() > MAX_REQUEST / 2)
        || (r.text.as_deref().unwrap_or("").is_empty()
            && r.html.as_deref().unwrap_or("").is_empty())
        || files.len() > MAX_ATTACHMENTS
        || total > MAX_REQUEST
    {
        return Err(if total > MAX_REQUEST {
            ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Message payload is too large",
                None,
            )
        } else {
            bad()
        });
    }
    for address in r.to.iter().chain(&r.cc).chain(&r.bcc) {
        if !valid_email(address) {
            return Err(bad());
        }
    }
    for a in files {
        if a.filename.is_empty()
            || a.filename.len() > 255
            || a.filename.chars().any(char::is_control)
            || a.content_type.len() > 255
            || a.content_type.contains(['\r', '\n'])
        {
            return Err(bad());
        }
    }
    Ok(())
}
fn valid_email(s: &str) -> bool {
    s.len() <= 320 && lettre::Address::from_str(s).is_ok()
}
fn validate_plugin_input(
    r: &SendRequest,
    plugin: &PluginInvocation,
    files: &[PluginAttachment],
) -> Result<(), ApiError> {
    if plugin.plugin_id.is_empty()
        || plugin.plugin_id.len() > 128
        || !plugin
            .plugin_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || !plugin.input.is_object()
        || r.to.len() + r.cc.len() + r.bcc.len() > 100
        || r.to
            .iter()
            .chain(&r.cc)
            .chain(&r.bcc)
            .any(|a| !valid_email(a))
        || files.len() > MAX_ATTACHMENTS
    {
        return Err(bad());
    }
    Ok(())
}
fn make_draft(
    r: &SendRequest,
    files: &[PluginAttachment],
    identity: Option<&Identity>,
) -> Result<MailDraft, ApiError> {
    let mailbox = |name: Option<String>, address: String| Mailbox { name, address };
    let mut recipients = Vec::new();
    recipients.extend(r.to.iter().map(|a| Recipient::To(mailbox(None, a.clone()))));
    recipients.extend(r.cc.iter().map(|a| Recipient::Cc(mailbox(None, a.clone()))));
    recipients.extend(
        r.bcc
            .iter()
            .map(|a| Recipient::Bcc(mailbox(None, a.clone()))),
    );
    Ok(MailDraft {
        sender_identity_id: r.sender_identity_id.clone(),
        from: identity.map(|i| mailbox(Some(i.display_name.clone()), i.email_address.clone())),
        reply_to: None,
        recipients,
        subject: r.subject.clone(),
        text_body: r.text.clone(),
        html_body: r.html.clone(),
        attachments: files.to_vec(),
    })
}
fn recipient_audit(draft: &MailDraft) -> RecipientAudit {
    let mut audit = RecipientAudit {
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
    };
    for recipient in &draft.recipients {
        match recipient {
            Recipient::To(mailbox) => audit.to.push(mailbox.address.clone()),
            Recipient::Cc(mailbox) => audit.cc.push(mailbox.address.clone()),
            Recipient::Bcc(mailbox) => audit.bcc.push(mailbox.address.clone()),
        }
    }
    audit
}
fn request_digest(
    mode: &str,
    draft: &MailDraft,
    source: Option<&str>,
) -> Result<Vec<u8>, ApiError> {
    let bytes = match source {
        Some(plugin_request) => serde_json::to_vec(&(mode, "plugin", plugin_request)),
        None => serde_json::to_vec(&(mode, draft)),
    }
    .map_err(|_| internal())?;
    Ok(Sha256::digest(bytes).to_vec())
}

async fn replay_existing(
    state: &AppState,
    token_id: &str,
    mode: &str,
    digest: &[u8],
    existing: (Vec<u8>, String, String, String),
) -> Result<(StatusCode, Json<SendResponse>), ApiError> {
    if existing.0 != digest || existing.1 != mode {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "Idempotency-Key was used with a different request",
            Some(existing.2),
        ));
    }
    if existing.3 == "processing" {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "message_processing",
            "The original request is still processing",
            Some(existing.2),
        ));
    }
    if mode == "sync" && existing.3 != "smtp_accepted" {
        let (http, code) = match existing.3.as_str() {
            "rejected" => (StatusCode::UNPROCESSABLE_ENTITY, "message_rejected"),
            "outcome_unknown" => (StatusCode::SERVICE_UNAVAILABLE, "outcome_unknown"),
            _ => (StatusCode::BAD_GATEWAY, "message_failed"),
        };
        return Err(ApiError(
            http,
            code,
            "The original message did not succeed",
            Some(existing.2),
        ));
    }
    let accepted_at = if mode == "sync" {
        fetch_status(state, &existing.2, Some(token_id), false)
            .await?
            .and_then(|v| v.finished_at)
    } else {
        None
    };
    Ok((
        if mode == "async" {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        Json(SendResponse {
            message_id: existing.2.clone(),
            status: existing.3,
            status_url: format!("/api/v1/messages/{}", existing.2),
            smtp_accepted_at: accepted_at,
        }),
    ))
}

async fn find_idempotent(
    db: &Database,
    token: &str,
    key: &str,
) -> Result<Option<(Vec<u8>, String, String, String)>, ApiError> {
    let row=match db {Database::Sqlite(p)=>mapdb(sqlx::query_as::<_,(Vec<u8>,String,String,String)>("SELECT request_digest,mode,id,status FROM messages WHERE token_id=? AND idempotency_key=?").bind(token).bind(key).fetch_optional(p).await)?,Database::Mysql(p)=>mapdb(sqlx::query_as::<_,(Vec<u8>,String,String,String)>("SELECT request_digest,mode,id,status FROM messages WHERE token_id=? AND idempotency_key=?").bind(token).bind(key).fetch_optional(p).await)?};
    Ok(row)
}
async fn insert_message(
    s: &AppState,
    token: &str,
    id: &str,
    sender: &str,
    version: i64,
    mode: &str,
    key: &str,
    digest: &[u8],
    recipients: &[u8],
    created: &str,
    expires: Option<&str>,
    status: &str,
    payload: Option<&[u8]>,
) -> Result<bool, ApiError> {
    match &s.database {
        Database::Sqlite(p) => {
            let mut tx = mapdb(p.begin().await)?;
            let inserted=sqlx::query("INSERT INTO messages(id,token_id,sender_identity_id,identity_version,mode,status,idempotency_key,request_digest,recipient_ciphertext,created_at,queued_at,expires_at,content_retained_until,audit_retained_until) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,NULL,NULL)").bind(id).bind(token).bind(sender).bind(version).bind(mode).bind(status).bind(key).bind(digest).bind(recipients).bind(created).bind(Option::<&str>::None).bind(expires).execute(&mut *tx).await;
            if let Err(e) = inserted {
                if matches!(&e,sqlx::Error::Database(d) if d.is_unique_violation()) {
                    return Ok(false);
                }
                tracing::error!(error=%e,"message insert failed");
                return Err(internal());
            }
            if let Some(cipher) = payload {
                mapdb(sqlx::query("INSERT INTO message_payloads(message_id,payload_ciphertext,key_version,retained_until) VALUES(?,?,1,NULL)").bind(id).bind(cipher).execute(&mut *tx).await)?;
            }
            mapdb(tx.commit().await)?;
        }
        Database::Mysql(p) => {
            let mut tx = mapdb(p.begin().await)?;
            let created = DateTime::parse_from_rfc3339(created)
                .map_err(|_| internal())?
                .naive_utc();
            let expires = expires
                .map(|v| {
                    DateTime::parse_from_rfc3339(v)
                        .map(|d| d.naive_utc())
                        .map_err(|_| internal())
                })
                .transpose()?;
            let inserted=sqlx::query("INSERT INTO messages(id,token_id,sender_identity_id,identity_version,mode,status,idempotency_key,request_digest,recipient_ciphertext,created_at,queued_at,expires_at,content_retained_until,audit_retained_until) VALUES(?,?,?,?,?,?,?,?,?,?,NULL,?,NULL,NULL)").bind(id).bind(token).bind(sender).bind(version as u64).bind(mode).bind(status).bind(key).bind(digest).bind(recipients).bind(created).bind(expires).execute(&mut *tx).await;
            if let Err(e) = inserted {
                if matches!(&e,sqlx::Error::Database(d) if d.is_unique_violation()) {
                    return Ok(false);
                }
                tracing::error!(error=%e,"message insert failed");
                return Err(internal());
            }
            if let Some(cipher) = payload {
                mapdb(sqlx::query("INSERT INTO message_payloads(message_id,payload_ciphertext,key_version,retained_until) VALUES(?,?,1,NULL)").bind(id).bind(cipher).execute(&mut *tx).await)?;
            }
            mapdb(tx.commit().await)?;
        }
    }
    Ok(true)
}
async fn insert_event(
    db: &Database,
    id: &str,
    event: &str,
    plugin: Option<&str>,
    details: Option<&str>,
) -> Result<(), ApiError> {
    let now = Utc::now();
    match db {
        Database::Sqlite(p) => {
            mapdb(sqlx::query("INSERT INTO message_events(id,message_id,event_type,plugin_id,created_at,safe_details) VALUES(?,?,?,?,?,?)").bind(Uuid::new_v4().to_string()).bind(id).bind(event).bind(plugin).bind(now.to_rfc3339()).bind(details).execute(p).await)?;
        }
        Database::Mysql(p) => {
            mapdb(sqlx::query("INSERT INTO message_events(id,message_id,event_type,plugin_id,created_at,safe_details) VALUES(?,?,?,?,?,?)").bind(Uuid::new_v4().to_string()).bind(id).bind(event).bind(plugin).bind(now.naive_utc()).bind(details).execute(p).await)?;
        }
    }
    Ok(())
}

pub async fn process_async(s: &AppState, id: &str) -> Result<(), String> {
    match process_message(s, id, true, None).await {
        Ok(_) => Ok(()),
        Err(error) => {
            let _ = mark_inflight_unknown(s, id).await;
            Err(format!("{}", error.1))
        }
    }
}
async fn mark_inflight_unknown(s: &AppState, id: &str) -> Result<(), ApiError> {
    let n = match &s.database {
        Database::Sqlite(p) => mapdb(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM message_attempts WHERE message_id=? AND finished_at IS NULL",
            )
            .bind(id)
            .fetch_one(p)
            .await,
        )?,
        Database::Mysql(p) => mapdb(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM message_attempts WHERE message_id=? AND finished_at IS NULL",
            )
            .bind(id)
            .fetch_one(p)
            .await,
        )?,
    };
    if n > 0 {
        finish(s, id, "outcome_unknown", "smtp_outcome_unknown").await?;
    }
    Ok(())
}
async fn process_message(
    s: &AppState,
    id: &str,
    worker: bool,
    transient: Option<MailDraft>,
) -> Result<Option<(StatusCode, &'static str, &'static str)>, ApiError> {
    let row = load_process_row(&s.database, id).await?;
    let Some((token, sender, version, mode, payload_cipher, recipients_cipher)) = row else {
        return Err(internal());
    };
    let mut payload = if let Some(draft) = transient {
        MessagePayload {
            draft,
            before_send_complete: false,
        }
    } else {
        let payload_cipher = payload_cipher.ok_or_else(internal)?;
        let bytes = s.crypto.decrypt(&payload_cipher).map_err(|_| internal())?;
        serde_json::from_slice::<MessagePayload>(&bytes).map_err(|_| internal())?
    };
    let _recipient_bytes = s
        .crypto
        .decrypt(&recipients_cipher)
        .map_err(|_| internal())?;
    let permitted = crate::tokens::authorize_identity(&s.database, &token, &sender)
        .await
        .map_err(|_| internal())?;
    let Some(mut identity) = load_identity(&s.database, &s.crypto, &sender).await? else {
        finish(s, id, "failed", "sender_unavailable").await?;
        return Ok(Some((
            StatusCode::FORBIDDEN,
            "sender_unavailable",
            "Sender identity is unavailable",
        )));
    };
    if !permitted || identity.version != version {
        finish(s, id, "failed", "sender_authorization_changed").await?;
        return Ok(Some((
            StatusCode::FORBIDDEN,
            "sender_unavailable",
            "Sender identity authorization changed",
        )));
    }
    if payload.draft.sender_identity_id != sender {
        finish(s, id, "failed", "sender_identity_changed").await?;
        return Ok(Some((
            StatusCode::FORBIDDEN,
            "sender_unavailable",
            "Sender identity is unavailable",
        )));
    }
    if !payload.before_send_complete {
        match s.plugins.before_send(id, payload.draft.clone()).await {
            Ok(FlowOutcome::Continue { draft }) => payload.draft = draft,
            Ok(FlowOutcome::Reject { code, plugin_id }) => {
                let code = safe_plugin_code(&code);
                finish(s, id, "rejected", "plugin_rejected").await?;
                insert_event(
                    &s.database,
                    id,
                    "plugin_rejected",
                    Some(&plugin_id),
                    Some(&code),
                )
                .await?;
                return Ok(Some((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "message_rejected",
                    "Message was rejected by policy",
                )));
            }
            Ok(FlowOutcome::Hold {
                draft,
                plugin_id,
                resume_at,
            }) => {
                if worker {
                    let still_allowed =
                        crate::tokens::authorize_identity(&s.database, &token, &sender)
                            .await
                            .map_err(|_| internal())?;
                    let current = load_identity(&s.database, &s.crypto, &sender).await?;
                    if !still_allowed || current.as_ref().is_none_or(|i| i.version != version) {
                        finish(s, id, "failed", "sender_authorization_changed").await?;
                        return Ok(Some((
                            StatusCode::FORBIDDEN,
                            "sender_unavailable",
                            "Sender identity authorization changed",
                        )));
                    }
                    if validate_draft(&draft, &identity).is_err() {
                        finish(s, id, "failed", "plugin_invalid_draft").await?;
                        return Err(bad());
                    }
                    payload.draft = draft;
                    persist_payload(s, id, &payload).await?;
                    set_held(s, id, resume_at.as_deref()).await?;
                    insert_event(&s.database, id, "held", Some(&plugin_id), None).await?;
                    return Ok(None);
                }
                finish(s, id, "rejected", "requires_async").await?;
                insert_event(&s.database, id, "requires_async", Some(&plugin_id), None).await?;
                return Ok(Some((
                    StatusCode::CONFLICT,
                    "requires_async",
                    "Message requires asynchronous processing",
                )));
            }
            Err(e) => {
                tracing::error!(message_id=%id,error=%e,"before_send plugin failed");
                finish(s, id, "failed", "plugin_error").await?;
                return Ok(Some((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "plugin_error",
                    "Message processing failed",
                )));
            }
        }
        payload.before_send_complete = true;
    }
    // Plugin drafts are untrusted input. Revalidate sender, recipients and all header/body bounds.
    let still_permitted = crate::tokens::authorize_identity(&s.database, &token, &sender)
        .await
        .map_err(|_| internal())?;
    identity = load_identity(&s.database, &s.crypto, &sender)
        .await?
        .ok_or_else(internal)?;
    if !still_permitted || identity.version != version {
        finish(s, id, "failed", "sender_authorization_changed").await?;
        return Ok(Some((
            StatusCode::FORBIDDEN,
            "sender_unavailable",
            "Sender identity authorization changed",
        )));
    }
    if validate_draft(&payload.draft, &identity).is_err() {
        finish(s, id, "failed", "plugin_invalid_draft").await?;
        return Ok(Some((
            StatusCode::UNPROCESSABLE_ENTITY,
            "plugin_error",
            "Message processing failed",
        )));
    }
    if mode == "async" {
        persist_payload(s, id, &payload).await?;
    }
    let smtp = match make_smtp_settings(&identity) {
        Ok(value) => value,
        Err(error) => {
            finish(s, id, "failed", "smtp_settings_invalid").await?;
            return Err(error);
        }
    };
    let mail = match to_smtp_message(&payload.draft, &identity) {
        Ok(value) => value,
        Err(error) => {
            finish(s, id, "failed", "message_invalid").await?;
            return Err(error);
        }
    };
    let (attempt, attempt_no) = begin_attempt(&s.database, id).await?;
    let result = match smtp_transport::send(&smtp, &mail).await {
        Ok(result) => result,
        Err(_) => {
            finish(s, id, "failed", "smtp_submission_failed").await?;
            complete_attempt(
                &s.database,
                &attempt,
                "permanent_failure",
                Some("smtp_submission_failed"),
            )
            .await?;
            return Ok(Some((
                StatusCode::BAD_GATEWAY,
                "smtp_failed",
                "SMTP submission failed",
            )));
        }
    };
    let delivery = match result {
        smtp_transport::SendOutcome::Accepted => {
            finish(s, id, "smtp_accepted", "smtp_accepted").await?;
            complete_attempt(&s.database, &attempt, "accepted", None).await?;
            DeliveryResult::Accepted
        }
        smtp_transport::SendOutcome::TransientRejection if worker && attempt_no < 3 => {
            complete_attempt(
                &s.database,
                &attempt,
                "temporary_failure",
                Some("smtp_temporary_rejection"),
            )
            .await?;
            schedule_retry(&s, id, attempt_no).await?;
            insert_event(
                &s.database,
                id,
                "retry_scheduled",
                None,
                Some("smtp_temporary_rejection"),
            )
            .await?;
            DeliveryResult::TemporaryFailure {
                safe_code: "smtp_temporary_rejection".to_owned(),
            }
        }
        smtp_transport::SendOutcome::TransientRejection
        | smtp_transport::SendOutcome::PermanentRejection => {
            finish(s, id, "failed", "smtp_rejected").await?;
            complete_attempt(
                &s.database,
                &attempt,
                "permanent_failure",
                Some("smtp_rejected"),
            )
            .await?;
            DeliveryResult::PermanentFailure {
                safe_code: "smtp_rejected".to_owned(),
            }
        }
        smtp_transport::SendOutcome::OutcomeUnknown => {
            finish(s, id, "outcome_unknown", "smtp_outcome_unknown").await?;
            complete_attempt(
                &s.database,
                &attempt,
                "unknown",
                Some("smtp_outcome_unknown"),
            )
            .await?;
            DeliveryResult::Unknown
        }
    };
    let api_result = match &delivery {
        DeliveryResult::Accepted => None,
        DeliveryResult::TemporaryFailure { .. } if worker && attempt_no < 3 => None,
        DeliveryResult::Unknown => Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "outcome_unknown",
            "SMTP delivery result could not be confirmed",
        )),
        _ => Some((
            StatusCode::BAD_GATEWAY,
            "smtp_failed",
            "SMTP submission failed",
        )),
    };
    s.plugins.after_result(id, &payload.draft, &delivery).await;
    if api_result.is_some() {
        return Ok(api_result);
    }
    Ok(None)
}

#[derive(FromRow)]
struct ProcessSqlite {
    token_id: String,
    sender_identity_id: String,
    identity_version: i64,
    mode: String,
    recipient_ciphertext: Vec<u8>,
    payload_ciphertext: Option<Vec<u8>>,
}
#[derive(FromRow)]
struct ProcessMysql {
    token_id: String,
    sender_identity_id: String,
    identity_version: u64,
    mode: String,
    recipient_ciphertext: Vec<u8>,
    payload_ciphertext: Option<Vec<u8>>,
}
async fn load_process_row(
    db: &Database,
    id: &str,
) -> Result<Option<(String, String, i64, String, Option<Vec<u8>>, Vec<u8>)>, ApiError> {
    match db {
        Database::Sqlite(p) => {
            let r=mapdb(sqlx::query_as::<_,ProcessSqlite>("SELECT m.token_id,m.sender_identity_id,m.identity_version,m.mode,m.recipient_ciphertext,p.payload_ciphertext FROM messages m LEFT JOIN message_payloads p ON p.message_id=m.id WHERE m.id=? AND m.status='processing'").bind(id).fetch_optional(p).await)?;
            Ok(r.map(|x| {
                (
                    x.token_id,
                    x.sender_identity_id,
                    x.identity_version,
                    x.mode,
                    x.payload_ciphertext,
                    x.recipient_ciphertext,
                )
            }))
        }
        Database::Mysql(p) => {
            let r=mapdb(sqlx::query_as::<_,ProcessMysql>("SELECT m.token_id,m.sender_identity_id,m.identity_version,m.mode,m.recipient_ciphertext,p.payload_ciphertext FROM messages m LEFT JOIN message_payloads p ON p.message_id=m.id WHERE m.id=? AND m.status='processing'").bind(id).fetch_optional(p).await)?;
            Ok(r.map(|x| {
                (
                    x.token_id,
                    x.sender_identity_id,
                    x.identity_version as i64,
                    x.mode,
                    x.payload_ciphertext,
                    x.recipient_ciphertext,
                )
            }))
        }
    }
}
async fn persist_payload(s: &AppState, id: &str, payload: &MessagePayload) -> Result<(), ApiError> {
    let bytes = serde_json::to_vec(payload).map_err(|_| internal())?;
    let cipher = s.crypto.encrypt(&bytes).map_err(|_| internal())?;
    match &s.database {
        Database::Sqlite(p) => {
            mapdb(
                sqlx::query("UPDATE message_payloads SET payload_ciphertext=? WHERE message_id=?")
                    .bind(cipher)
                    .bind(id)
                    .execute(p)
                    .await,
            )?;
        }
        Database::Mysql(p) => {
            mapdb(
                sqlx::query("UPDATE message_payloads SET payload_ciphertext=? WHERE message_id=?")
                    .bind(cipher)
                    .bind(id)
                    .execute(p)
                    .await,
            )?;
        }
    }
    Ok(())
}
async fn set_queued(s: &AppState, id: &str) -> Result<(), ApiError> {
    let now = Utc::now();
    match &s.database {
        Database::Sqlite(p) => {
            mapdb(sqlx::query("UPDATE messages SET status='queued',queued_at=? WHERE id=? AND status='processing'").bind(now.to_rfc3339()).bind(id).execute(p).await)?;
        }
        Database::Mysql(p) => {
            mapdb(sqlx::query("UPDATE messages SET status='queued',queued_at=? WHERE id=? AND status='processing'").bind(now.naive_utc()).bind(id).execute(p).await)?;
        }
    }
    Ok(())
}
async fn set_held(s: &AppState, id: &str, resume: Option<&str>) -> Result<(), ApiError> {
    let now = Utc::now();
    match &s.database {
        Database::Sqlite(p) => {
            mapdb(sqlx::query("UPDATE messages SET status='held',next_attempt_at=? WHERE id=? AND status='processing'").bind(resume.map(str::to_owned)).bind(id).execute(p).await)?;
        }
        Database::Mysql(p) => {
            let time = resume
                .map(DateTime::parse_from_rfc3339)
                .transpose()
                .map_err(|_| bad())?
                .map(|d| d.naive_utc());
            mapdb(sqlx::query("UPDATE messages SET status='held',next_attempt_at=? WHERE id=? AND status='processing'").bind(time).bind(id).execute(p).await)?;
        }
    }
    let _ = now;
    Ok(())
}
async fn finish(s: &AppState, id: &str, status: &str, code: &str) -> Result<(), ApiError> {
    let now = Utc::now();
    let content = (now + chrono::Duration::days(s.retention.content_days)).to_rfc3339();
    let audit = (now + chrono::Duration::days(s.retention.audit_days)).to_rfc3339();
    let error_code = (status != "smtp_accepted").then_some(code);
    match &s.database {
        Database::Sqlite(p) => {
            mapdb(sqlx::query("UPDATE messages SET status=?,finished_at=?,content_retained_until=?,audit_retained_until=?,last_error_code=? WHERE id=? AND status IN ('processing','queued','held')").bind(status).bind(now.to_rfc3339()).bind(&content).bind(&audit).bind(error_code).bind(id).execute(p).await)?;
            mapdb(
                sqlx::query("UPDATE message_payloads SET retained_until=? WHERE message_id=?")
                    .bind(content)
                    .bind(id)
                    .execute(p)
                    .await,
            )?;
        }
        Database::Mysql(p) => {
            let content = (now + chrono::Duration::days(s.retention.content_days)).naive_utc();
            mapdb(sqlx::query("UPDATE messages SET status=?,finished_at=?,content_retained_until=?,audit_retained_until=?,last_error_code=? WHERE id=? AND status IN ('processing','queued','held')").bind(status).bind(now.naive_utc()).bind(content).bind((now+chrono::Duration::days(s.retention.audit_days)).naive_utc()).bind(error_code).bind(id).execute(p).await)?;
            mapdb(
                sqlx::query("UPDATE message_payloads SET retained_until=? WHERE message_id=?")
                    .bind(content)
                    .bind(id)
                    .execute(p)
                    .await,
            )?;
        }
    }
    insert_event(&s.database, id, "status_changed", None, error_code).await?;
    Ok(())
}
async fn begin_attempt(db: &Database, id: &str) -> Result<(String, u32), ApiError> {
    let attempt = Uuid::new_v4().to_string();
    let now = Utc::now();
    let next = match db {
        Database::Sqlite(p) => mapdb(
            sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(attempt_no),0)+1 FROM message_attempts WHERE message_id=?",
            )
            .bind(id)
            .fetch_one(p)
            .await,
        )? as u32,
        Database::Mysql(p) => mapdb(
            sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(attempt_no),0)+1 FROM message_attempts WHERE message_id=?",
            )
            .bind(id)
            .fetch_one(p)
            .await,
        )? as u32,
    };
    match db {
        Database::Sqlite(p) => {
            mapdb(sqlx::query("INSERT INTO message_attempts(id,message_id,attempt_no,started_at,smtp_stage) VALUES(?,?,?,?, 'connect')").bind(&attempt).bind(id).bind(next as i64).bind(now.to_rfc3339()).execute(p).await)?;
        }
        Database::Mysql(p) => {
            mapdb(sqlx::query("INSERT INTO message_attempts(id,message_id,attempt_no,started_at,smtp_stage) VALUES(?,?,?,?, 'connect')").bind(&attempt).bind(id).bind(next).bind(now.naive_utc()).execute(p).await)?;
        }
    }
    Ok((attempt, next))
}
async fn schedule_retry(s: &AppState, id: &str, attempt: u32) -> Result<(), ApiError> {
    let delay = 30_i64.saturating_mul(
        1_i64
            .checked_shl(attempt.saturating_sub(1))
            .unwrap_or(i64::MAX),
    );
    let when = (Utc::now() + chrono::Duration::seconds(delay)).to_rfc3339();
    match &s.database {
        Database::Sqlite(p) => {
            mapdb(sqlx::query("UPDATE messages SET status='retry_wait',next_attempt_at=?,last_error_code='smtp_temporary_rejection' WHERE id=? AND status='processing'").bind(when).bind(id).execute(p).await)?;
        }
        Database::Mysql(p) => {
            let time = DateTime::parse_from_rfc3339(&when)
                .map_err(|_| internal())?
                .naive_utc();
            mapdb(sqlx::query("UPDATE messages SET status='retry_wait',next_attempt_at=?,last_error_code='smtp_temporary_rejection' WHERE id=? AND status='processing'").bind(time).bind(id).execute(p).await)?;
        }
    }
    Ok(())
}
async fn complete_attempt(
    db: &Database,
    id: &str,
    result: &str,
    code: Option<&str>,
) -> Result<(), ApiError> {
    let now = Utc::now();
    match db {
        Database::Sqlite(p) => {
            mapdb(sqlx::query("UPDATE message_attempts SET finished_at=?,result=?,safe_error_code=? WHERE id=?").bind(now.to_rfc3339()).bind(result).bind(code).bind(id).execute(p).await)?;
        }
        Database::Mysql(p) => {
            mapdb(sqlx::query("UPDATE message_attempts SET finished_at=?,result=?,safe_error_code=? WHERE id=?").bind(now.naive_utc()).bind(result).bind(code).bind(id).execute(p).await)?;
        }
    }
    Ok(())
}
fn make_smtp_settings(i: &Identity) -> Result<smtp_transport::SmtpSettings, ApiError> {
    let tls = match i.tls_mode.as_str() {
        "implicit" => smtp_transport::TlsMode::Implicit,
        "starttls" => smtp_transport::TlsMode::StartTls,
        "none" => smtp_transport::TlsMode::None,
        _ => return Err(internal()),
    };
    let auth = match i.auth_type.as_str() {
        "none" => None,
        "login" | "plain" => Some(smtp_transport::SmtpAuth {
            username: i.username.clone().ok_or_else(internal)?,
            password: i.credential.clone().ok_or_else(internal)?,
            mechanism: if i.auth_type == "login" {
                smtp_transport::AuthMechanism::Login
            } else {
                smtp_transport::AuthMechanism::Plain
            },
        }),
        _ => return Err(internal()),
    };
    Ok(smtp_transport::SmtpSettings {
        host: i.host.clone(),
        port: i.port,
        tls,
        auth,
    })
}
fn validate_draft(d: &MailDraft, i: &Identity) -> Result<(), ApiError> {
    let expected_from = Mailbox {
        name: Some(i.display_name.clone()),
        address: i.email_address.clone(),
    };
    if d.sender_identity_id != i.id
        || d.from.as_ref() != Some(&expected_from)
        || d.subject.is_empty()
        || d.subject.len() > 998
        || d.subject.contains(['\r', '\n'])
        || (d.text_body.as_deref().unwrap_or("").is_empty()
            && d.html_body.as_deref().unwrap_or("").is_empty())
        || d.recipients.is_empty()
    {
        return Err(bad());
    }
    for r in &d.recipients {
        let m = match r {
            Recipient::To(m) | Recipient::Cc(m) | Recipient::Bcc(m) => m,
        };
        if !valid_email(&m.address)
            || m.name
                .as_deref()
                .is_some_and(|n| n.chars().any(char::is_control))
        {
            return Err(bad());
        }
    }
    if d.reply_to.as_ref().is_some_and(|m| {
        !valid_email(&m.address)
            || m.name
                .as_deref()
                .is_some_and(|n| n.chars().any(char::is_control))
    }) {
        return Err(bad());
    }
    if d.attachments.len() > MAX_ATTACHMENTS {
        return Err(bad());
    }
    let mut total = 0usize;
    for a in &d.attachments {
        if a.filename.is_empty()
            || a.filename.len() > 255
            || a.filename.chars().any(char::is_control)
            || a.content_type.is_empty()
            || a.content_type.len() > 255
            || a.content_type.contains(['\r', '\n'])
        {
            return Err(bad());
        }
        let bytes = STANDARD.decode(&a.data_base64).map_err(|_| bad())?;
        if bytes.len() > MAX_ATTACHMENT {
            return Err(ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Attachment is too large",
                None,
            ));
        }
        total = total.saturating_add(bytes.len());
    }
    if total > MAX_REQUEST {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "Message payload is too large",
            None,
        ));
    }
    Ok(())
}
fn safe_plugin_code(code: &str) -> String {
    if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        code.to_owned()
    } else {
        "plugin_rejected".to_owned()
    }
}
fn to_smtp_message(d: &MailDraft, i: &Identity) -> Result<smtp_transport::Message, ApiError> {
    let from = d.from.as_ref().ok_or_else(bad)?;
    let format = |m: &Mailbox| {
        if let Some(name) = &m.name {
            format!("{name} <{}>", m.address)
        } else {
            m.address.clone()
        }
    };
    let mut to = Vec::new();
    let mut cc = Vec::new();
    let mut bcc = Vec::new();
    for r in &d.recipients {
        match r {
            Recipient::To(m) => to.push(format(m)),
            Recipient::Cc(m) => cc.push(format(m)),
            Recipient::Bcc(m) => bcc.push(format(m)),
        }
    }
    let attachments = d
        .attachments
        .iter()
        .map(|a| {
            Ok(smtp_transport::Attachment {
                filename: a.filename.clone(),
                content_type: a.content_type.clone(),
                bytes: STANDARD.decode(&a.data_base64).map_err(|_| bad())?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(smtp_transport::Message {
        from: format(from),
        envelope_from: Some(i.envelope_from.clone()),
        to,
        cc,
        bcc,
        subject: d.subject.clone(),
        text: d.text_body.clone(),
        html: d.html_body.clone(),
        attachments,
    })
}

async fn get_message(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<MessageView>, ApiError> {
    let token = authenticate(&s, &headers).await?;
    let view = fetch_status(&s, &id, Some(&token.id), false)
        .await?
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "not_found",
            "Message not found",
            None,
        ))?;
    Ok(Json(view))
}
#[derive(FromRow)]
struct StatusSqlite {
    id: String,
    token_id: String,
    mode: String,
    sender_identity_id: String,
    status: String,
    created_at: String,
    finished_at: Option<String>,
    recipient_ciphertext: Vec<u8>,
    attempt_count: i64,
    last_error_code: Option<String>,
}
#[derive(FromRow)]
struct StatusMysql {
    id: String,
    token_id: String,
    mode: String,
    sender_identity_id: String,
    status: String,
    created_at: NaiveDateTime,
    finished_at: Option<NaiveDateTime>,
    recipient_ciphertext: Vec<u8>,
    attempt_count: i64,
    last_error_code: Option<String>,
}
async fn fetch_status(
    s: &AppState,
    id: &str,
    token: Option<&str>,
    admin: bool,
) -> Result<Option<MessageView>, ApiError> {
    let row=match &s.database{Database::Sqlite(p)=>mapdb(sqlx::query_as::<_,StatusSqlite>("SELECT m.id,m.token_id,m.mode,m.sender_identity_id,m.status,m.created_at,m.finished_at,m.recipient_ciphertext,(SELECT COUNT(*) FROM message_attempts a WHERE a.message_id=m.id) AS attempt_count,m.last_error_code FROM messages m WHERE m.id=?").bind(id).fetch_optional(p).await)?.map(|r|(r.id,r.token_id,r.mode,r.sender_identity_id,r.status,r.created_at,r.finished_at,r.recipient_ciphertext,r.attempt_count,r.last_error_code)),Database::Mysql(p)=>mapdb(sqlx::query_as::<_,StatusMysql>("SELECT m.id,m.token_id,m.mode,m.sender_identity_id,m.status,m.created_at,m.finished_at,m.recipient_ciphertext,(SELECT COUNT(*) FROM message_attempts a WHERE a.message_id=m.id) AS attempt_count,m.last_error_code FROM messages m WHERE m.id=?").bind(id).fetch_optional(p).await)?.map(|r|(r.id,r.token_id,r.mode,r.sender_identity_id,r.status,r.created_at.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),r.finished_at.map(|t|t.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()),r.recipient_ciphertext,r.attempt_count,r.last_error_code))};
    let Some((mid, owner, mode, sender, status, created, finished, cipher, attempts, error)) = row
    else {
        return Ok(None);
    };
    if !admin && token != Some(owner.as_str()) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "forbidden",
            "This message belongs to another token",
            Some(mid),
        ));
    }
    let recipients = if admin || token == Some(owner.as_str()) {
        let bytes = s.crypto.decrypt(&cipher).map_err(|_| internal())?;
        let input: RecipientAudit = serde_json::from_slice(&bytes).map_err(|_| internal())?;
        Some(
            input
                .to
                .into_iter()
                .chain(input.cc)
                .chain(input.bcc)
                .collect(),
        )
    } else {
        None
    };
    Ok(Some(MessageView {
        message_id: mid,
        mode,
        sender_identity_id: sender,
        status,
        created_at: created,
        finished_at: finished,
        attempt_count: attempts,
        last_error_code: error,
        recipients,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        config::{Config, DatabaseKind, RetentionPolicy},
        database::Database,
        plugin_runtime::PluginManager,
    };
    use axum::{
        body::{to_bytes, Body},
        http::{Method, Request},
    };
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::{json, Value};
    use std::{path::PathBuf, sync::Arc};
    use tower::ServiceExt;

    struct TestResolver;

    #[postwarden_plugin_api::async_trait]
    impl postwarden_plugin_api::Plugin for TestResolver {
        fn id(&self) -> &'static str {
            "test-resolver"
        }
        fn version(&self) -> &'static str {
            "1"
        }
        async fn compose(
            &self,
            draft: &MailDraft,
            input: &Value,
            _context: &postwarden_plugin_api::PluginContext,
        ) -> Result<postwarden_plugin_api::HookDecision, postwarden_plugin_api::PluginError>
        {
            if input.get("scenario_id").and_then(Value::as_str) != Some("custom.alert") {
                return Ok(postwarden_plugin_api::HookDecision::Reject {
                    code: "unknown_scenario".into(),
                });
            }
            let mut changed = draft.clone();
            changed.sender_identity_id = "ident_test".into();
            changed.recipients = vec![Recipient::To(Mailbox {
                name: None,
                address: "rcpt@example.org".into(),
            })];
            changed.subject = "Custom alert".into();
            changed.text_body = Some("Handled by plugin".into());
            Ok(postwarden_plugin_api::HookDecision::Modify { draft: changed })
        }
    }

    async fn fixture() -> (axum::Router, String, String, String, Database, AppState) {
        let config = Config {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            database_kind: DatabaseKind::Sqlite,
            database_url: "sqlite::memory:".into(),
            web_dist: PathBuf::from("frontend/dist"),
            cookie_secure: false,
            retention: RetentionPolicy {
                pending_days: 7,
                content_days: 7,
                audit_days: 90,
            },
        };
        let db = Database::connect(&config).await.unwrap();
        let crypto = crate::crypto::Crypto::new([3; 32]);
        let plugins = PluginManager::new(db.clone(), crypto.clone())
            .await
            .unwrap();
        let account = "acct_test".to_string();
        let identity = "ident_test".to_string();
        let token_id = "token_test".to_string();
        let raw = format!("pwt_0123456789abcdef_{}", URL_SAFE_NO_PAD.encode([7u8; 32]));
        let digest = Sha256::digest(raw.as_bytes()).to_vec();
        let now = Utc::now().to_rfc3339();
        if let Database::Sqlite(p) = &db {
            sqlx::query("INSERT INTO smtp_accounts(id,name,host,port,tls_mode,auth_type,enabled,created_at,updated_at) VALUES(?, 'test','localhost',25,'none','none',1,?,?)").bind(&account).bind(&now).bind(&now).execute(p).await.unwrap();
            sqlx::query("INSERT INTO sender_identities(id,smtp_account_id,display_name,email_address,envelope_from,version,enabled,created_at,updated_at) VALUES(?,?, 'Test','sender@example.org','sender@example.org',1,1,?,?)").bind(&identity).bind(&account).bind(&now).bind(&now).execute(p).await.unwrap();
            sqlx::query("INSERT INTO api_tokens(id,name,lookup_prefix,secret_digest,status,created_at) VALUES(?,'test','0123456789abcdef',?,'enabled',?)").bind(&token_id).bind(digest).bind(&now).execute(p).await.unwrap();
            sqlx::query("INSERT INTO token_sender_permissions(token_id,sender_identity_id,created_at) VALUES(?,?,?)").bind(&token_id).bind(&identity).bind(&now).execute(p).await.unwrap();
        }
        let state = AppState {
            database: db.clone(),
            crypto,
            cookie_secure: false,
            plugins,
            retention: config.retention,
        };
        let app = Router::new()
            .nest("/api/v1", super::router())
            .with_state(state.clone());
        (app, raw, token_id, identity, db, state)
    }
    fn body(identity: &str, subject: &str) -> Value {
        json!({"sender_identity_id":identity,"to":["rcpt@example.org"],"subject":subject,"text":"hello"})
    }
    async fn call(
        app: &axum::Router,
        method: Method,
        path: &str,
        raw: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> axum::response::Response {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {raw}"));
        if let Some(k) = key {
            b = b.header("idempotency-key", k)
        }
        let payload = if let Some(v) = body {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        } else {
            Body::empty()
        };
        app.clone().oneshot(b.body(payload).unwrap()).await.unwrap()
    }
    #[tokio::test]
    async fn token_authorization_idempotency_and_async_status() {
        let (app, raw, token, identity, _db, _state) = fixture().await;
        let denied = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("key-denied"),
            Some(body("not-authorized", "hello")),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let first = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("same-key"),
            Some(body(&identity, "hello")),
        )
        .await;
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let first_json: Value =
            serde_json::from_slice(&to_bytes(first.into_body(), MAX_REQUEST).await.unwrap())
                .unwrap();
        let id = first_json["message_id"].as_str().unwrap().to_owned();
        assert_eq!(first_json["status"], "queued");
        let duplicate = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("same-key"),
            Some(body(&identity, "hello")),
        )
        .await;
        assert_eq!(duplicate.status(), StatusCode::ACCEPTED);
        let duplicate_json: Value =
            serde_json::from_slice(&to_bytes(duplicate.into_body(), MAX_REQUEST).await.unwrap())
                .unwrap();
        assert_eq!(duplicate_json["message_id"], id);
        let conflict = call(
            &app,
            Method::POST,
            "/api/v1/messages/send",
            &raw,
            Some("same-key"),
            Some(body(&identity, "changed")),
        )
        .await;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let status = call(
            &app,
            Method::GET,
            &format!("/api/v1/messages/{id}"),
            &raw,
            None,
            None,
        )
        .await;
        assert_eq!(status.status(), StatusCode::OK);
        if let Database::Sqlite(p) = &_db {
            sqlx::query(
                "DELETE FROM token_sender_permissions WHERE token_id=? AND sender_identity_id=?",
            )
            .bind(&token)
            .bind(&identity)
            .execute(p)
            .await
            .unwrap();
            sqlx::query("UPDATE messages SET status='processing' WHERE id=?")
                .bind(&id)
                .execute(p)
                .await
                .unwrap();
        }
        process_async(&_state, &id).await.unwrap();
        let status = if let Database::Sqlite(p) = &_db {
            sqlx::query_scalar::<_, String>("SELECT status FROM messages WHERE id=?")
                .bind(&id)
                .fetch_one(p)
                .await
                .unwrap()
        } else {
            unreachable!()
        };
        assert_eq!(status, "failed");
    }

    #[tokio::test]
    async fn template_plugin_uses_existing_send_pipeline_and_can_be_disabled() {
        let (app, raw, token, identity, db, state) = fixture().await;
        let now = Utc::now().to_rfc3339();
        if let Database::Sqlite(pool) = &db {
            sqlx::query("INSERT INTO mail_templates(id,scenario_id,name,enabled,allow_caller_recipients,sender_identity_id,subject_template,text_template,html_template,to_json,cc_json,bcc_json,version,created_at,updated_at) VALUES('tpl_test','order.paid','Order paid',1,0,?,'Order {{order_id}}','Paid {{order_id}}',NULL,'[\"rcpt@example.org\"]','[]','[]',1,?,?)")
                .bind(&identity).bind(&now).bind(&now).execute(pool).await.unwrap();
        }
        let payload = json!({
            "sender_identity_id":"",
            "to":[],
            "subject":"",
            "text":null,
            "plugin_id":"mail-template",
            "plugin_input":{"scenario_id":"order.paid","variables":{"order_id":"A1001"}}
        });
        let response = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("template-1"),
            Some(payload.clone()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let first: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), MAX_REQUEST).await.unwrap())
                .unwrap();
        let id = first["message_id"].as_str().unwrap();
        let recorded = fetch_status(&state, id, Some(&token), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recorded.recipients.unwrap(), vec!["rcpt@example.org"]);
        if let Database::Sqlite(pool) = &db {
            let cipher: Vec<u8> = sqlx::query_scalar(
                "SELECT payload_ciphertext FROM message_payloads WHERE message_id=?",
            )
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
            let payload: MessagePayload =
                serde_json::from_slice(&state.crypto.decrypt(&cipher).unwrap()).unwrap();
            assert_eq!(payload.draft.subject, "Order A1001");
            assert_eq!(payload.draft.text_body.as_deref(), Some("Paid A1001"));
        }
        let duplicate = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("template-1"),
            Some(payload),
        )
        .await;
        assert_eq!(duplicate.status(), StatusCode::ACCEPTED);
        let second: Value =
            serde_json::from_slice(&to_bytes(duplicate.into_body(), MAX_REQUEST).await.unwrap())
                .unwrap();
        assert_eq!(second["message_id"], id);
        let extra_recipient = call(&app, Method::POST, "/api/v1/messages", &raw, Some("template-2"), Some(json!({"to":["extra@example.org"],"plugin_id":"mail-template","plugin_input":{"scenario_id":"order.paid","variables":{"order_id":"A1002"}}}))).await;
        assert_eq!(extra_recipient.status(), StatusCode::ACCEPTED);
        let extra: Value = serde_json::from_slice(
            &to_bytes(extra_recipient.into_body(), MAX_REQUEST)
                .await
                .unwrap(),
        )
        .unwrap();
        let extra_recorded = fetch_status(
            &state,
            extra["message_id"].as_str().unwrap(),
            Some(&token),
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(extra_recorded.recipients.unwrap(), vec!["rcpt@example.org"]);
        let old_envelope = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("template-old-envelope"),
            Some(json!({"template":{"plugin_id":"mail-template","input":{"scenario_id":"order.paid"}}})),
        )
        .await;
        assert_eq!(old_envelope.status(), StatusCode::BAD_REQUEST);
        let input_without_plugin = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("template-no-plugin"),
            Some(json!({"plugin_input":{"scenario_id":"order.paid"}})),
        )
        .await;
        assert_eq!(input_without_plugin.status(), StatusCode::BAD_REQUEST);
        let null_input_without_plugin = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("template-null-input"),
            Some(json!({"plugin_input":null})),
        )
        .await;
        assert_eq!(null_input_without_plugin.status(), StatusCode::BAD_REQUEST);
        let plugin_without_input = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("template-no-input"),
            Some(json!({"plugin_id":"mail-template"})),
        )
        .await;
        assert_eq!(plugin_without_input.status(), StatusCode::BAD_REQUEST);
        let missing = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("template-3"),
            Some(json!({"plugin_id":"mail-template","plugin_input":{"scenario_id":"order.paid"}})),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::UNPROCESSABLE_ENTITY);
        if let Database::Sqlite(pool) = &db {
            sqlx::query("UPDATE plugin_configs SET enabled=0 WHERE plugin_id='mail-template'")
                .execute(pool)
                .await
                .unwrap();
        }
        let disabled = call(&app, Method::POST, "/api/v1/messages", &raw, Some("template-disabled"), Some(json!({"plugin_id":"mail-template","plugin_input":{"scenario_id":"order.paid","variables":{"order_id":"A1003"}}}))).await;
        assert_eq!(disabled.status(), StatusCode::CONFLICT);
        if let Database::Sqlite(pool) = &db {
            sqlx::query("DELETE FROM mail_templates WHERE id='tpl_test'")
                .execute(pool)
                .await
                .unwrap();
        }
        let retry = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("template-1"),
            Some(json!({"sender_identity_id":"","to":[],"subject":"","text":null,"plugin_id":"mail-template","plugin_input":{"scenario_id":"order.paid","variables":{"order_id":"A1001"}}})),
        )
        .await;
        assert_eq!(retry.status(), StatusCode::ACCEPTED);
        let retried: Value =
            serde_json::from_slice(&to_bytes(retry.into_body(), MAX_REQUEST).await.unwrap())
                .unwrap();
        assert_eq!(retried["message_id"], id);
    }

    #[tokio::test]
    async fn a_custom_plugin_can_compose_a_message() {
        let (_app, raw, _token, _identity, db, mut state) = fixture().await;
        state.plugins =
            PluginManager::with_plugins(db, state.crypto.clone(), vec![Arc::new(TestResolver)])
                .await
                .unwrap();
        let app = Router::new()
            .nest("/api/v1", super::router())
            .with_state(state);
        let response = call(
            &app,
            Method::POST,
            "/api/v1/messages",
            &raw,
            Some("plugin-event-1"),
            Some(
                json!({"plugin_id":"test-resolver","plugin_input":{"scenario_id":"custom.alert"}}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
}
