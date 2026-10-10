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
        .route("/smtp-accounts", get(list_accounts).post(create_account))
        .route(
            "/smtp-accounts/{id}",
            get(get_account).put(update_account).delete(delete_account),
        )
        .route("/smtp-accounts/{id}/enable", post(enable_account))
        .route("/smtp-accounts/{id}/disable", post(disable_account))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAccount {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls_mode: String,
    pub auth_type: String,
    pub username: Option<String>,
    pub credential: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateAccount {
    pub name: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub tls_mode: Option<String>,
    pub auth_type: Option<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct AccountView {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls_mode: String,
    pub auth_type: String,
    pub username: Option<String>,
    pub has_credentials: bool,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(FromRow)]
struct AccountRow {
    id: String,
    name: String,
    host: String,
    port: i64,
    tls_mode: String,
    auth_type: String,
    username: Option<String>,
    credential_ciphertext: Option<Vec<u8>>,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

#[derive(FromRow)]
struct MysqlAccountRow {
    id: String,
    name: String,
    host: String,
    port: u16,
    tls_mode: String,
    auth_type: String,
    username: Option<String>,
    credential_ciphertext: Option<Vec<u8>>,
    enabled: bool,
    created_at: NaiveDateTime,
    updated_at: NaiveDateTime,
}

impl From<AccountRow> for AccountView {
    fn from(row: AccountRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            host: row.host,
            port: row.port as u16,
            tls_mode: row.tls_mode,
            auth_type: row.auth_type,
            username: row.username,
            has_credentials: row.credential_ciphertext.is_some(),
            enabled: row.enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<MysqlAccountRow> for AccountView {
    fn from(row: MysqlAccountRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            host: row.host,
            port: row.port,
            tls_mode: row.tls_mode,
            auth_type: row.auth_type,
            username: row.username,
            has_credentials: row.credential_ciphertext.is_some(),
            enabled: row.enabled,
            created_at: row.created_at.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
            updated_at: row.updated_at.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
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

fn bad_request() -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "SMTP account settings are invalid",
    )
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not_found", "SMTP account not found")
}

fn internal() -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Request could not be completed",
    )
}

fn validate(
    name: &str,
    host: &str,
    port: u16,
    tls: &str,
    auth: &str,
    username: Option<&str>,
    credential: Option<&str>,
) -> ApiResult<()> {
    if name.trim().is_empty()
        || name.len() > 191
        || host.trim().is_empty()
        || host.len() > 255
        || host.chars().any(char::is_whitespace)
        || port == 0
        || !matches!(tls, "implicit" | "starttls" | "none")
        || !matches!(auth, "none" | "login" | "plain")
    {
        return Err(bad_request());
    }
    if auth == "none" {
        if username.is_some() || credential.is_some() {
            return Err(bad_request());
        }
    } else if tls == "none"
        || username.is_none_or(|s| s.trim().is_empty() || s.len() > 320)
        || credential.is_none_or(|s| s.is_empty() || s.len() > 4096)
    {
        return Err(bad_request());
    }
    Ok(())
}

fn map_db<T>(result: Result<T, sqlx::Error>) -> ApiResult<T> {
    result.map_err(|error| {
        tracing::error!(error = %error, "SMTP account database operation failed");
        internal()
    })
}

async fn list_accounts(
    _: AdminAuth,
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<AccountView>>> {
    let rows = match &state.database {
        Database::Sqlite(pool) => map_db(sqlx::query_as::<_, AccountRow>("SELECT id,name,host,port,tls_mode,auth_type,username,credential_ciphertext,enabled,created_at,updated_at FROM smtp_accounts WHERE deleted_at IS NULL ORDER BY created_at DESC").fetch_all(pool).await)?,
        Database::Mysql(pool) => map_db(sqlx::query_as::<_, MysqlAccountRow>("SELECT id,name,host,port,tls_mode,auth_type,username,credential_ciphertext,enabled,created_at,updated_at FROM smtp_accounts WHERE deleted_at IS NULL ORDER BY created_at DESC").fetch_all(pool).await)?.into_iter().map(AccountRow::from).collect(),
    };
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

impl From<MysqlAccountRow> for AccountRow {
    fn from(row: MysqlAccountRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            host: row.host,
            port: row.port as i64,
            tls_mode: row.tls_mode,
            auth_type: row.auth_type,
            username: row.username,
            credential_ciphertext: row.credential_ciphertext,
            enabled: row.enabled,
            created_at: row.created_at.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
            updated_at: row.updated_at.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
        }
    }
}

async fn get_account(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<AccountView>> {
    let row = fetch_account(&state.database, &id)
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(row.into()))
}

async fn fetch_account(db: &Database, id: &str) -> ApiResult<Option<AccountRow>> {
    match db {
        Database::Sqlite(pool) => map_db(sqlx::query_as::<_, AccountRow>("SELECT id,name,host,port,tls_mode,auth_type,username,credential_ciphertext,enabled,created_at,updated_at FROM smtp_accounts WHERE id=? AND deleted_at IS NULL").bind(id).fetch_optional(pool).await),
        Database::Mysql(pool) => Ok(map_db(sqlx::query_as::<_, MysqlAccountRow>("SELECT id,name,host,port,tls_mode,auth_type,username,credential_ciphertext,enabled,created_at,updated_at FROM smtp_accounts WHERE id=? AND deleted_at IS NULL").bind(id).fetch_optional(pool).await)?.map(Into::into)),
    }
}

async fn create_account(
    _: AdminAuth,
    State(state): State<AppState>,
    Json(input): Json<CreateAccount>,
) -> ApiResult<(StatusCode, Json<AccountView>)> {
    let name = input.name.trim();
    let host = input.host.trim();
    validate(
        name,
        host,
        input.port,
        &input.tls_mode,
        &input.auth_type,
        input.username.as_deref(),
        input.credential.as_deref(),
    )?;
    let ciphertext = match input.credential.as_deref() {
        Some(value) => Some(
            state
                .crypto
                .encrypt(value.as_bytes())
                .map_err(|_| internal())?,
        ),
        None => None,
    };
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let view = match &state.database {
        Database::Sqlite(pool) => {
            let timestamp = now.to_rfc3339();
            map_db(sqlx::query("INSERT INTO smtp_accounts(id,name,host,port,tls_mode,auth_type,username,credential_ciphertext,enabled,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
                .bind(&id).bind(name).bind(host).bind(input.port as i64).bind(&input.tls_mode).bind(&input.auth_type).bind(input.username.as_deref()).bind(ciphertext).bind(input.enabled.unwrap_or(true)).bind(&timestamp).bind(&timestamp).execute(pool).await)?;
            fetch_account(&state.database, &id)
                .await?
                .ok_or_else(internal)?
                .into()
        }
        Database::Mysql(pool) => {
            let timestamp = now.naive_utc();
            map_db(sqlx::query("INSERT INTO smtp_accounts(id,name,host,port,tls_mode,auth_type,username,credential_ciphertext,enabled,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
                .bind(&id).bind(name).bind(host).bind(input.port).bind(&input.tls_mode).bind(&input.auth_type).bind(input.username.as_deref()).bind(ciphertext).bind(input.enabled.unwrap_or(true)).bind(timestamp).bind(timestamp).execute(pool).await)?;
            fetch_account(&state.database, &id)
                .await?
                .ok_or_else(internal)?
                .into()
        }
    };
    Ok((StatusCode::CREATED, Json(view)))
}

async fn update_account(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateAccount>,
) -> ApiResult<Json<AccountView>> {
    let current = fetch_account(&state.database, &id)
        .await?
        .ok_or_else(not_found)?;
    let name = input.name.as_deref().unwrap_or(&current.name).trim();
    let host = input.host.as_deref().unwrap_or(&current.host).trim();
    let port = input.port.unwrap_or(current.port as u16);
    let tls = input.tls_mode.as_deref().unwrap_or(&current.tls_mode);
    let auth = input.auth_type.as_deref().unwrap_or(&current.auth_type);
    let username = if auth == "none" {
        None
    } else {
        input.username.as_deref().or(current.username.as_deref())
    };
    let new_credential = input.credential.as_deref();
    if auth == "none" {
        if input.username.is_some() || new_credential.is_some() {
            return Err(bad_request());
        }
        validate(name, host, port, tls, auth, None, None)?;
    } else {
        let has_saved_credential = current.credential_ciphertext.is_some();
        if new_credential.is_none() && (!has_saved_credential || auth != current.auth_type) {
            return Err(bad_request());
        }
        validate(
            name,
            host,
            port,
            tls,
            auth,
            username,
            new_credential.or(Some("preserved")),
        )?;
    }
    let ciphertext = if auth == "none" {
        None
    } else if let Some(value) = new_credential {
        Some(
            state
                .crypto
                .encrypt(value.as_bytes())
                .map_err(|_| internal())?,
        )
    } else {
        current.credential_ciphertext.clone()
    };
    let now = Utc::now();
    match &state.database {
        Database::Sqlite(pool) => {
            map_db(sqlx::query("UPDATE smtp_accounts SET name=?,host=?,port=?,tls_mode=?,auth_type=?,username=?,credential_ciphertext=?,enabled=?,updated_at=? WHERE id=? AND deleted_at IS NULL").bind(name).bind(host).bind(port as i64).bind(tls).bind(auth).bind(username).bind(ciphertext).bind(input.enabled.unwrap_or(current.enabled)).bind(now.to_rfc3339()).bind(&id).execute(pool).await)?;
        }
        Database::Mysql(pool) => {
            map_db(sqlx::query("UPDATE smtp_accounts SET name=?,host=?,port=?,tls_mode=?,auth_type=?,username=?,credential_ciphertext=?,enabled=?,updated_at=? WHERE id=? AND deleted_at IS NULL").bind(name).bind(host).bind(port).bind(tls).bind(auth).bind(username).bind(ciphertext).bind(input.enabled.unwrap_or(current.enabled)).bind(now.naive_utc()).bind(&id).execute(pool).await)?;
        }
    }
    Ok(Json(
        fetch_account(&state.database, &id)
            .await?
            .ok_or_else(not_found)?
            .into(),
    ))
}

async fn delete_account(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if fetch_account(&state.database, &id).await?.is_none() {
        return Err(not_found());
    }
    let now = Utc::now();
    match &state.database {
        Database::Sqlite(pool) => {
            map_db(sqlx::query("UPDATE smtp_accounts SET enabled=0,auth_type='none',credential_ciphertext=NULL,username=NULL,deleted_at=?,updated_at=? WHERE id=? AND deleted_at IS NULL").bind(now.to_rfc3339()).bind(now.to_rfc3339()).bind(id).execute(pool).await)?;
        }
        Database::Mysql(pool) => {
            map_db(sqlx::query("UPDATE smtp_accounts SET enabled=FALSE,auth_type='none',credential_ciphertext=NULL,username=NULL,deleted_at=?,updated_at=? WHERE id=? AND deleted_at IS NULL").bind(now.naive_utc()).bind(now.naive_utc()).bind(id).execute(pool).await)?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn enable_account(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<AccountView>> {
    set_enabled(&state.database, &id, true).await?;
    Ok(Json(
        fetch_account(&state.database, &id)
            .await?
            .ok_or_else(not_found)?
            .into(),
    ))
}

async fn disable_account(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<AccountView>> {
    set_enabled(&state.database, &id, false).await?;
    Ok(Json(
        fetch_account(&state.database, &id)
            .await?
            .ok_or_else(not_found)?
            .into(),
    ))
}

async fn set_enabled(db: &Database, id: &str, enabled: bool) -> ApiResult<()> {
    let now = Utc::now();
    let rows_affected = match db {
        Database::Sqlite(pool) => map_db(
            sqlx::query(
                "UPDATE smtp_accounts SET enabled=?,updated_at=? WHERE id=? AND deleted_at IS NULL",
            )
            .bind(enabled)
            .bind(now.to_rfc3339())
            .bind(id)
            .execute(pool)
            .await,
        )?
        .rows_affected(),
        Database::Mysql(pool) => map_db(
            sqlx::query(
                "UPDATE smtp_accounts SET enabled=?,updated_at=? WHERE id=? AND deleted_at IS NULL",
            )
            .bind(enabled)
            .bind(now.naive_utc())
            .bind(id)
            .execute(pool)
            .await,
        )?
        .rows_affected(),
    };
    if rows_affected == 0 {
        return Err(not_found());
    }
    Ok(())
}
