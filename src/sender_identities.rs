use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{auth::AdminAuth, database::Database, http::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sender-identities", get(list).post(create))
        .route(
            "/sender-identities/{id}",
            get(get_one).put(update).delete(soft_delete),
        )
        .route("/sender-identities/{id}/enable", post(enable))
        .route("/sender-identities/{id}/disable", post(disable))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateIdentity {
    pub smtp_account_id: String,
    pub display_name: String,
    pub email_address: String,
    pub envelope_from: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateIdentity {
    pub smtp_account_id: Option<String>,
    pub display_name: Option<String>,
    pub email_address: Option<String>,
    pub envelope_from: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct IdentityView {
    pub id: String,
    pub smtp_account_id: String,
    pub display_name: String,
    pub email_address: String,
    pub envelope_from: String,
    pub version: i64,
    pub enabled: bool,
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(FromRow)]
struct IdentityRow {
    id: String,
    smtp_account_id: String,
    display_name: String,
    email_address: String,
    envelope_from: String,
    version: i64,
    enabled: bool,
    deleted_at: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(FromRow)]
struct MysqlIdentityRow {
    id: String,
    smtp_account_id: String,
    display_name: String,
    email_address: String,
    envelope_from: String,
    version: i64,
    enabled: bool,
    deleted_at: Option<NaiveDateTime>,
    created_at: NaiveDateTime,
    updated_at: NaiveDateTime,
}

impl From<MysqlIdentityRow> for IdentityRow {
    fn from(row: MysqlIdentityRow) -> Self {
        let format = |time: NaiveDateTime| time.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string();
        Self {
            id: row.id,
            smtp_account_id: row.smtp_account_id,
            display_name: row.display_name,
            email_address: row.email_address,
            envelope_from: row.envelope_from,
            version: row.version,
            enabled: row.enabled,
            deleted_at: row.deleted_at.map(format),
            created_at: format(row.created_at),
            updated_at: format(row.updated_at),
        }
    }
}

impl From<IdentityRow> for IdentityView {
    fn from(row: IdentityRow) -> Self {
        Self {
            id: row.id,
            smtp_account_id: row.smtp_account_id,
            display_name: row.display_name,
            email_address: row.email_address,
            envelope_from: row.envelope_from,
            version: row.version,
            enabled: row.enabled,
            deleted_at: row.deleted_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

type ApiResult<T> = Result<T, ApiError>;
struct ApiError(StatusCode, &'static str, &'static str);

impl axum::response::IntoResponse for ApiError {
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

fn invalid() -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Sender identity settings are invalid",
    )
}
fn missing() -> ApiError {
    ApiError(
        StatusCode::NOT_FOUND,
        "not_found",
        "Sender identity or active SMTP account not found",
    )
}
fn internal() -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Request could not be completed",
    )
}
fn map_db<T>(result: Result<T, sqlx::Error>) -> ApiResult<T> {
    result.map_err(|error| {
        tracing::error!(error = %error, "sender identity database operation failed");
        internal()
    })
}

// Keep the accepted syntax intentionally conservative: a single mailbox with
// no display-name syntax, whitespace, control bytes, or SMTP header delimiters.
fn valid_address(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 320
        || value
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || local.len() > 64
        || domain.len() > 255
    {
        return false;
    }
    if local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return false;
    }
    local
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn validate(display_name: &str, email: &str, envelope: &str) -> ApiResult<()> {
    if display_name.trim().is_empty()
        || display_name.len() > 191
        || display_name.chars().any(char::is_control)
        || !valid_address(email)
        || !valid_address(envelope)
    {
        return Err(invalid());
    }
    Ok(())
}

// A blank envelope sender means the visible sender address should receive
// delivery failure notices too. On update, omission keeps the current value.
fn resolved_envelope<'a>(
    submitted: Option<&'a str>,
    email: &'a str,
    current: Option<&'a str>,
) -> &'a str {
    match submitted {
        Some(value) if !value.trim().is_empty() => value.trim(),
        Some(_) => email,
        None => current.unwrap_or(email),
    }
}

async fn require_active_account(db: &Database, id: &str) -> ApiResult<()> {
    let active = match db {
        Database::Sqlite(pool) => map_db(
            sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM smtp_accounts WHERE id=? AND enabled=1 AND deleted_at IS NULL",
            )
            .bind(id)
            .fetch_optional(pool)
            .await,
        )?
        .is_some(),
        Database::Mysql(pool) => map_db(
            sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM smtp_accounts WHERE id=? AND enabled=TRUE AND deleted_at IS NULL",
            )
            .bind(id)
            .fetch_optional(pool)
            .await,
        )?
        .is_some(),
    };
    if active {
        Ok(())
    } else {
        Err(missing())
    }
}

async fn list(_: AdminAuth, State(state): State<AppState>) -> ApiResult<Json<Vec<IdentityView>>> {
    let rows = match &state.database {
        Database::Sqlite(pool) => map_db(sqlx::query_as::<_, IdentityRow>("SELECT id,smtp_account_id,display_name,email_address,envelope_from,version,enabled,deleted_at,created_at,updated_at FROM sender_identities ORDER BY created_at DESC").fetch_all(pool).await)?,
        Database::Mysql(pool) => map_db(sqlx::query_as::<_, MysqlIdentityRow>("SELECT id,smtp_account_id,display_name,email_address,envelope_from,version,enabled,deleted_at,created_at,updated_at FROM sender_identities ORDER BY created_at DESC").fetch_all(pool).await)?.into_iter().map(Into::into).collect(),
    };
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

async fn fetch(db: &Database, id: &str) -> ApiResult<Option<IdentityRow>> {
    match db {
        Database::Sqlite(pool) => map_db(sqlx::query_as::<_, IdentityRow>("SELECT id,smtp_account_id,display_name,email_address,envelope_from,version,enabled,deleted_at,created_at,updated_at FROM sender_identities WHERE id=?").bind(id).fetch_optional(pool).await),
        Database::Mysql(pool) => Ok(map_db(sqlx::query_as::<_, MysqlIdentityRow>("SELECT id,smtp_account_id,display_name,email_address,envelope_from,version,enabled,deleted_at,created_at,updated_at FROM sender_identities WHERE id=?").bind(id).fetch_optional(pool).await)?.map(Into::into)),
    }
}

async fn get_one(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<IdentityView>> {
    Ok(Json(
        fetch(&state.database, &id)
            .await?
            .ok_or_else(missing)?
            .into(),
    ))
}

async fn create(
    _: AdminAuth,
    State(state): State<AppState>,
    Json(input): Json<CreateIdentity>,
) -> ApiResult<(StatusCode, Json<IdentityView>)> {
    let name = input.display_name.trim();
    let email = input.email_address.trim();
    let envelope = resolved_envelope(input.envelope_from.as_deref(), email, None);
    validate(name, email, envelope)?;
    require_active_account(&state.database, &input.smtp_account_id).await?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    match &state.database {
        Database::Sqlite(pool) => {
            let ts = now.to_rfc3339();
            map_db(sqlx::query("INSERT INTO sender_identities(id,smtp_account_id,display_name,email_address,envelope_from,version,enabled,created_at,updated_at) VALUES(?,?,?,?,?,1,?,?,?)").bind(&id).bind(input.smtp_account_id).bind(name).bind(email).bind(envelope).bind(input.enabled.unwrap_or(true)).bind(&ts).bind(&ts).execute(pool).await)?;
        }
        Database::Mysql(pool) => {
            let ts = now.naive_utc();
            map_db(sqlx::query("INSERT INTO sender_identities(id,smtp_account_id,display_name,email_address,envelope_from,version,enabled,created_at,updated_at) VALUES(?,?,?,?,?,1,?,?,?)").bind(&id).bind(input.smtp_account_id).bind(name).bind(email).bind(envelope).bind(input.enabled.unwrap_or(true)).bind(ts).bind(ts).execute(pool).await)?;
        }
    }
    Ok((
        StatusCode::CREATED,
        Json(
            fetch(&state.database, &id)
                .await?
                .ok_or_else(internal)?
                .into(),
        ),
    ))
}

async fn update(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateIdentity>,
) -> ApiResult<Json<IdentityView>> {
    let current = fetch(&state.database, &id).await?.ok_or_else(missing)?;
    if current.deleted_at.is_some() {
        return Err(missing());
    }
    let account = input
        .smtp_account_id
        .as_deref()
        .unwrap_or(&current.smtp_account_id);
    let name = input
        .display_name
        .as_deref()
        .unwrap_or(&current.display_name)
        .trim();
    let email = input
        .email_address
        .as_deref()
        .unwrap_or(&current.email_address)
        .trim();
    let envelope = resolved_envelope(
        input.envelope_from.as_deref(),
        email,
        Some(&current.envelope_from),
    );
    validate(name, email, envelope)?;
    require_active_account(&state.database, account).await?;
    let changed = account != current.smtp_account_id
        || name != current.display_name
        || email != current.email_address
        || envelope != current.envelope_from;
    let version = current.version + i64::from(changed);
    let now = Utc::now();
    match &state.database {
        Database::Sqlite(pool) => {
            map_db(sqlx::query("UPDATE sender_identities SET smtp_account_id=?,display_name=?,email_address=?,envelope_from=?,version=?,enabled=?,updated_at=? WHERE id=? AND deleted_at IS NULL").bind(account).bind(name).bind(email).bind(envelope).bind(version).bind(input.enabled.unwrap_or(current.enabled)).bind(now.to_rfc3339()).bind(&id).execute(pool).await)?;
        }
        Database::Mysql(pool) => {
            map_db(sqlx::query("UPDATE sender_identities SET smtp_account_id=?,display_name=?,email_address=?,envelope_from=?,version=?,enabled=?,updated_at=? WHERE id=? AND deleted_at IS NULL").bind(account).bind(name).bind(email).bind(envelope).bind(version).bind(input.enabled.unwrap_or(current.enabled)).bind(now.naive_utc()).bind(&id).execute(pool).await)?;
        }
    }
    Ok(Json(
        fetch(&state.database, &id)
            .await?
            .ok_or_else(missing)?
            .into(),
    ))
}

async fn soft_delete(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if fetch(&state.database, &id)
        .await?
        .is_none_or(|row| row.deleted_at.is_some())
    {
        return Err(missing());
    }
    let now = Utc::now();
    let affected = match &state.database {
        Database::Sqlite(pool) => map_db(sqlx::query("UPDATE sender_identities SET enabled=0,deleted_at=?,updated_at=? WHERE id=? AND deleted_at IS NULL").bind(now.to_rfc3339()).bind(now.to_rfc3339()).bind(id).execute(pool).await)?.rows_affected(),
        Database::Mysql(pool) => map_db(sqlx::query("UPDATE sender_identities SET enabled=FALSE,deleted_at=?,updated_at=? WHERE id=? AND deleted_at IS NULL").bind(now.naive_utc()).bind(now.naive_utc()).bind(id).execute(pool).await)?.rows_affected(),
    };
    if affected == 0 {
        return Err(missing());
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn enable(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<IdentityView>> {
    set_enabled(&state.database, &id, true).await?;
    Ok(Json(
        fetch(&state.database, &id)
            .await?
            .ok_or_else(missing)?
            .into(),
    ))
}
async fn disable(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<IdentityView>> {
    set_enabled(&state.database, &id, false).await?;
    Ok(Json(
        fetch(&state.database, &id)
            .await?
            .ok_or_else(missing)?
            .into(),
    ))
}
async fn set_enabled(db: &Database, id: &str, enabled: bool) -> ApiResult<()> {
    let current = fetch(db, id).await?.ok_or_else(missing)?;
    if current.deleted_at.is_some() {
        return Err(missing());
    }
    if enabled {
        require_active_account(db, &current.smtp_account_id).await?;
    }
    let now = Utc::now();
    let affected = match db {
        Database::Sqlite(pool) => map_db(sqlx::query("UPDATE sender_identities SET enabled=?,updated_at=? WHERE id=? AND deleted_at IS NULL").bind(enabled).bind(now.to_rfc3339()).bind(id).execute(pool).await)?.rows_affected(),
        Database::Mysql(pool) => map_db(sqlx::query("UPDATE sender_identities SET enabled=?,updated_at=? WHERE id=? AND deleted_at IS NULL").bind(enabled).bind(now.naive_utc()).bind(id).execute(pool).await)?.rows_affected(),
    };
    if affected == 0 {
        return Err(missing());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{resolved_envelope, valid_address, validate, CreateIdentity};

    #[test]
    fn address_validation_blocks_header_injection() {
        for address in [
            "a@example.com\r\nBcc:x@y.z",
            "<a@example.com>",
            "a@@example.com",
            "a@bad..example",
        ] {
            assert!(!valid_address(address), "accepted {address:?}");
        }
        assert!(valid_address("sender+tag@example.com"));
        assert!(validate("Example Sender", "sender@example.com", "bounce@example.com").is_ok());
    }

    #[test]
    fn blank_envelope_uses_sender_and_omitted_update_keeps_existing() {
        let input: CreateIdentity = serde_json::from_value(serde_json::json!({
            "smtp_account_id": "account-id",
            "display_name": "Test",
            "email_address": "new@example.com"
        }))
        .unwrap();
        assert!(input.envelope_from.is_none());
        assert_eq!(
            resolved_envelope(None, "new@example.com", None),
            "new@example.com"
        );
        assert_eq!(
            resolved_envelope(Some("  "), "new@example.com", None),
            "new@example.com"
        );
        assert_eq!(
            resolved_envelope(Some(""), "new@example.com", Some("old@example.com")),
            "new@example.com"
        );
        assert_eq!(
            resolved_envelope(None, "new@example.com", Some("old@example.com")),
            "old@example.com"
        );
        assert_eq!(
            resolved_envelope(Some(" bounce@example.com "), "new@example.com", None),
            "bounce@example.com"
        );
    }
}
