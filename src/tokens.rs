use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, NaiveDateTime, Utc};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{auth::AdminAuth, database::Database, http::AppState};

#[derive(Clone, Debug)]
pub struct TokenPrincipal {
    pub id: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/tokens", get(list_tokens).post(create_token))
        .route("/tokens/{id}", get(get_token).put(update_token))
        .route("/tokens/{id}/enable", post(enable_token))
        .route("/tokens/{id}/disable", post(disable_token))
        .route("/tokens/{id}/revoke", post(revoke_token))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateToken {
    name: String,
    purpose: Option<String>,
    expires_at: Option<String>,
    sender_identity_ids: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateToken {
    name: Option<String>,
    purpose: Option<serde_json::Value>,
    expires_at: Option<serde_json::Value>,
    sender_identity_ids: Option<Vec<String>>,
}
#[derive(Serialize)]
struct TokenView {
    id: String,
    name: String,
    purpose: Option<String>,
    lookup_prefix: String,
    status: String,
    expires_at: Option<String>,
    created_at: String,
    last_used_at: Option<String>,
    revoked_at: Option<String>,
    sender_identity_ids: Vec<String>,
}
#[derive(Serialize)]
struct CreatedToken {
    #[serde(flatten)]
    token: TokenView,
    token_secret: String,
}
#[derive(FromRow)]
struct TokenRow {
    id: String,
    name: String,
    purpose: Option<String>,
    lookup_prefix: String,
    status: String,
    expires_at: Option<String>,
    created_at: String,
    last_used_at: Option<String>,
    revoked_at: Option<String>,
}
#[derive(FromRow)]
struct MysqlTokenRow {
    id: String,
    name: String,
    purpose: Option<String>,
    lookup_prefix: String,
    status: String,
    expires_at: Option<NaiveDateTime>,
    created_at: NaiveDateTime,
    last_used_at: Option<NaiveDateTime>,
    revoked_at: Option<NaiveDateTime>,
}
impl From<MysqlTokenRow> for TokenRow {
    fn from(r: MysqlTokenRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            purpose: r.purpose,
            lookup_prefix: r.lookup_prefix,
            status: r.status,
            expires_at: r.expires_at.map(fmt_mysql),
            created_at: fmt_mysql(r.created_at),
            last_used_at: r.last_used_at.map(fmt_mysql),
            revoked_at: r.revoked_at.map(fmt_mysql),
        }
    }
}
fn fmt_mysql(d: NaiveDateTime) -> String {
    d.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
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
        "Token settings are invalid",
    )
}
fn missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not_found", "API token not found")
}
fn internal() -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Request could not be completed",
    )
}
fn map_db<T>(r: Result<T, sqlx::Error>) -> ApiResult<T> {
    r.map_err(|e| {
        tracing::error!(error=%e,"API token operation failed");
        internal()
    })
}

fn parse_expiry(v: Option<&str>) -> ApiResult<Option<DateTime<Utc>>> {
    v.map(|s| {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|_| invalid())
    })
    .transpose()
}
fn new_secret() -> (String, String, Vec<u8>) {
    let mut prefix = [0u8; 8];
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut prefix);
    OsRng.fill_bytes(&mut secret);
    let prefix = hex(&prefix);
    let raw = format!("pwt_{prefix}_{}", URL_SAFE_NO_PAD.encode(secret));
    let digest = Sha256::digest(raw.as_bytes()).to_vec();
    (prefix, raw, digest)
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}
fn valid_name(name: &str, purpose: Option<&str>) -> bool {
    !name.trim().is_empty() && name.len() <= 191 && purpose.is_none_or(|p| p.len() <= 255)
}
fn sqlite_time(d: DateTime<Utc>) -> String {
    d.to_rfc3339()
}

async fn identities_valid(db: &Database, ids: &[String]) -> Result<bool, sqlx::Error> {
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Ok(false);
        }
        let ok=match db {
            Database::Sqlite(p)=>sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM sender_identities i JOIN smtp_accounts a ON a.id=i.smtp_account_id WHERE i.id=? AND i.enabled=1 AND i.deleted_at IS NULL AND a.enabled=1 AND a.deleted_at IS NULL").bind(id).fetch_one(p).await? > 0,
            Database::Mysql(p)=>sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM sender_identities i JOIN smtp_accounts a ON a.id=i.smtp_account_id WHERE i.id=? AND i.enabled=TRUE AND i.deleted_at IS NULL AND a.enabled=TRUE AND a.deleted_at IS NULL").bind(id).fetch_one(p).await? > 0,
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}
async fn view(db: &Database, id: &str) -> ApiResult<Option<TokenView>> {
    let row=match db {
        Database::Sqlite(p)=>map_db(sqlx::query_as::<_,TokenRow>("SELECT id,name,purpose,lookup_prefix,status,expires_at,created_at,last_used_at,revoked_at FROM api_tokens WHERE id=?").bind(id).fetch_optional(p).await)?,
        Database::Mysql(p)=>map_db(sqlx::query_as::<_,MysqlTokenRow>("SELECT id,name,purpose,lookup_prefix,status,expires_at,created_at,last_used_at,revoked_at FROM api_tokens WHERE id=?").bind(id).fetch_optional(p).await)?.map(Into::into),
    };
    let Some(r) = row else { return Ok(None) };
    let ids=match db { Database::Sqlite(p)=>map_db(sqlx::query_scalar::<_,String>("SELECT sender_identity_id FROM token_sender_permissions WHERE token_id=? ORDER BY sender_identity_id").bind(id).fetch_all(p).await)?, Database::Mysql(p)=>map_db(sqlx::query_scalar::<_,String>("SELECT sender_identity_id FROM token_sender_permissions WHERE token_id=? ORDER BY sender_identity_id").bind(id).fetch_all(p).await)? };
    Ok(Some(TokenView {
        id: r.id,
        name: r.name,
        purpose: r.purpose,
        lookup_prefix: r.lookup_prefix,
        status: r.status,
        expires_at: r.expires_at,
        created_at: r.created_at,
        last_used_at: r.last_used_at,
        revoked_at: r.revoked_at,
        sender_identity_ids: ids,
    }))
}
async fn list_tokens(_: AdminAuth, State(s): State<AppState>) -> ApiResult<Json<Vec<TokenView>>> {
    let rows = match &s.database {
        Database::Sqlite(p) => map_db(
            sqlx::query_scalar::<_, String>("SELECT id FROM api_tokens ORDER BY created_at DESC")
                .fetch_all(p)
                .await,
        )?,
        Database::Mysql(p) => map_db(
            sqlx::query_scalar::<_, String>("SELECT id FROM api_tokens ORDER BY created_at DESC")
                .fetch_all(p)
                .await,
        )?,
    };
    let mut out = Vec::with_capacity(rows.len());
    for id in rows {
        if let Some(v) = view(&s.database, &id).await? {
            out.push(v)
        }
    }
    Ok(Json(out))
}
async fn get_token(
    _: AdminAuth,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<TokenView>> {
    Ok(Json(view(&s.database, &id).await?.ok_or_else(missing)?))
}
async fn create_token(
    _: AdminAuth,
    State(s): State<AppState>,
    Json(input): Json<CreateToken>,
) -> ApiResult<(StatusCode, Json<CreatedToken>)> {
    let name = input.name.trim();
    if !valid_name(name, input.purpose.as_deref())
        || !identities_valid(&s.database, &input.sender_identity_ids)
            .await
            .map_err(|_| internal())?
    {
        return Err(invalid());
    }
    let expiry = parse_expiry(input.expires_at.as_deref())?;
    if expiry.is_some_and(|e| e <= Utc::now()) {
        return Err(invalid());
    }
    let id = Uuid::new_v4().to_string();
    let (prefix, raw, digest) = new_secret();
    let now = Utc::now();
    let sqlite_now = sqlite_time(now);
    let mysql_now = now.naive_utc();
    match &s.database {
        Database::Sqlite(p) => {
            let mut tx = map_db(p.begin().await)?;
            map_db(sqlx::query("INSERT INTO api_tokens(id,name,purpose,lookup_prefix,secret_digest,status,expires_at,created_at) VALUES(?,?,?,?,?,'enabled',?,?)").bind(&id).bind(name).bind(&input.purpose).bind(&prefix).bind(&digest).bind(expiry.map(sqlite_time)).bind(&sqlite_now).execute(&mut *tx).await)?;
            for identity_id in &input.sender_identity_ids {
                map_db(sqlx::query("INSERT INTO token_sender_permissions(token_id,sender_identity_id,created_at) VALUES(?,?,?)").bind(&id).bind(identity_id).bind(&sqlite_now).execute(&mut *tx).await)?;
            }
            map_db(tx.commit().await)?;
        }
        Database::Mysql(p) => {
            let mut tx = map_db(p.begin().await)?;
            map_db(sqlx::query("INSERT INTO api_tokens(id,name,purpose,lookup_prefix,secret_digest,status,expires_at,created_at) VALUES(?,?,?,?,?,'enabled',?,?)").bind(&id).bind(name).bind(&input.purpose).bind(&prefix).bind(&digest).bind(expiry.map(|d|d.naive_utc())).bind(mysql_now).execute(&mut *tx).await)?;
            for identity_id in &input.sender_identity_ids {
                map_db(sqlx::query("INSERT INTO token_sender_permissions(token_id,sender_identity_id,created_at) VALUES(?,?,?)").bind(&id).bind(identity_id).bind(mysql_now).execute(&mut *tx).await)?;
            }
            map_db(tx.commit().await)?;
        }
    }
    let token = view(&s.database, &id).await?.ok_or_else(internal)?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            token,
            token_secret: raw,
        }),
    ))
}
async fn update_token(
    _: AdminAuth,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateToken>,
) -> ApiResult<Json<TokenView>> {
    let cur = view(&s.database, &id).await?.ok_or_else(missing)?;
    if cur.status == "revoked" {
        return Err(invalid());
    }
    let name = input.name.as_deref().unwrap_or(&cur.name).trim();
    let purpose = match input.purpose {
        None => cur.purpose.clone(),
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => Some(value),
        Some(_) => return Err(invalid()),
    };
    let expiry_str = match input.expires_at {
        None => cur.expires_at.clone(),
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => Some(value),
        Some(_) => return Err(invalid()),
    };
    let expiry = parse_expiry(expiry_str.as_deref())?;
    if !valid_name(name, purpose.as_deref()) || expiry.is_some_and(|e| e <= Utc::now()) {
        return Err(invalid());
    }
    let ids = input.sender_identity_ids.unwrap_or(cur.sender_identity_ids);
    if !identities_valid(&s.database, &ids)
        .await
        .map_err(|_| internal())?
    {
        return Err(invalid());
    }
    let now = Utc::now();
    match &s.database {
        Database::Sqlite(p) => {
            let mut tx = map_db(p.begin().await)?;
            map_db(sqlx::query("UPDATE api_tokens SET name=?,purpose=?,expires_at=? WHERE id=? AND status<>'revoked'").bind(name).bind(&purpose).bind(expiry.map(sqlite_time)).bind(&id).execute(&mut *tx).await)?;
            map_db(
                sqlx::query("DELETE FROM token_sender_permissions WHERE token_id=?")
                    .bind(&id)
                    .execute(&mut *tx)
                    .await,
            )?;
            for identity_id in &ids {
                map_db(sqlx::query("INSERT INTO token_sender_permissions(token_id,sender_identity_id,created_at) VALUES(?,?,?)").bind(&id).bind(identity_id).bind(sqlite_time(now)).execute(&mut *tx).await)?;
            }
            map_db(tx.commit().await)?;
        }
        Database::Mysql(p) => {
            let mut tx = map_db(p.begin().await)?;
            map_db(sqlx::query("UPDATE api_tokens SET name=?,purpose=?,expires_at=? WHERE id=? AND status<>'revoked'").bind(name).bind(&purpose).bind(expiry.map(|d|d.naive_utc())).bind(&id).execute(&mut *tx).await)?;
            map_db(
                sqlx::query("DELETE FROM token_sender_permissions WHERE token_id=?")
                    .bind(&id)
                    .execute(&mut *tx)
                    .await,
            )?;
            for identity_id in &ids {
                map_db(sqlx::query("INSERT INTO token_sender_permissions(token_id,sender_identity_id,created_at) VALUES(?,?,?)").bind(&id).bind(identity_id).bind(now.naive_utc()).execute(&mut *tx).await)?;
            }
            map_db(tx.commit().await)?;
        }
    }
    Ok(Json(view(&s.database, &id).await?.ok_or_else(missing)?))
}
async fn disable_token(
    _: AdminAuth,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<TokenView>> {
    set_status(&s.database, &id, "disabled", false).await?;
    Ok(Json(view(&s.database, &id).await?.ok_or_else(missing)?))
}
async fn enable_token(
    _: AdminAuth,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<TokenView>> {
    set_status(&s.database, &id, "enabled", false).await?;
    Ok(Json(view(&s.database, &id).await?.ok_or_else(missing)?))
}
async fn revoke_token(
    _: AdminAuth,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<TokenView>> {
    set_status(&s.database, &id, "revoked", true).await?;
    Ok(Json(view(&s.database, &id).await?.ok_or_else(missing)?))
}
async fn set_status(db: &Database, id: &str, status: &str, revoke: bool) -> ApiResult<()> {
    let now = Utc::now();
    let affected = match db {
        Database::Sqlite(p) => map_db(sqlx::query("UPDATE api_tokens SET status=?,revoked_at=CASE WHEN ? THEN ? ELSE revoked_at END WHERE id=? AND status<>'revoked'").bind(status).bind(revoke).bind(sqlite_time(now)).bind(id).execute(p).await)?.rows_affected(),
        Database::Mysql(p) => map_db(sqlx::query("UPDATE api_tokens SET status=?,revoked_at=CASE WHEN ? THEN ? ELSE revoked_at END WHERE id=? AND status<>'revoked'").bind(status).bind(revoke).bind(now.naive_utc()).bind(id).execute(p).await)?.rows_affected(),
    };
    if affected == 0 {
        return Err(missing());
    }
    Ok(())
}

/// Validate a presented token, update its last-used timestamp, and return its principal.
pub async fn authenticate(
    database: &Database,
    raw: &str,
) -> Result<Option<TokenPrincipal>, sqlx::Error> {
    let Some((prefix, _)) = raw.strip_prefix("pwt_").and_then(|s| s.split_once('_')) else {
        return Ok(None);
    };
    if prefix.len() != 16 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let digest = Sha256::digest(raw.as_bytes()).to_vec();
    let now = Utc::now();
    let id=match database {Database::Sqlite(p)=>sqlx::query_scalar::<_,String>("SELECT id FROM api_tokens WHERE lookup_prefix=? AND secret_digest=? AND status='enabled' AND (expires_at IS NULL OR expires_at>?)").bind(prefix).bind(&digest).bind(sqlite_time(now)).fetch_optional(p).await?,Database::Mysql(p)=>sqlx::query_scalar::<_,String>("SELECT id FROM api_tokens WHERE lookup_prefix=? AND secret_digest=? AND status='enabled' AND (expires_at IS NULL OR expires_at>?)").bind(prefix).bind(&digest).bind(now.naive_utc()).fetch_optional(p).await?};
    if let Some(id) = id {
        match database {
            Database::Sqlite(p) => {
                sqlx::query("UPDATE api_tokens SET last_used_at=? WHERE id=?")
                    .bind(sqlite_time(now))
                    .bind(&id)
                    .execute(p)
                    .await?;
            }
            Database::Mysql(p) => {
                sqlx::query("UPDATE api_tokens SET last_used_at=? WHERE id=?")
                    .bind(now.naive_utc())
                    .bind(&id)
                    .execute(p)
                    .await?;
            }
        }
        Ok(Some(TokenPrincipal { id }))
    } else {
        Ok(None)
    }
}
/// Check an explicit token grant and current sender/account availability.
pub async fn authorize_identity(
    database: &Database,
    token_id: &str,
    sender_identity_id: &str,
) -> Result<bool, sqlx::Error> {
    match database {Database::Sqlite(p)=>sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM token_sender_permissions p JOIN api_tokens t ON t.id=p.token_id JOIN sender_identities i ON i.id=p.sender_identity_id JOIN smtp_accounts a ON a.id=i.smtp_account_id WHERE p.token_id=? AND p.sender_identity_id=? AND t.status='enabled' AND (t.expires_at IS NULL OR t.expires_at>?) AND i.enabled=1 AND i.deleted_at IS NULL AND a.enabled=1 AND a.deleted_at IS NULL").bind(token_id).bind(sender_identity_id).bind(sqlite_time(Utc::now())).fetch_one(p).await.map(|n|n>0),Database::Mysql(p)=>sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM token_sender_permissions p JOIN api_tokens t ON t.id=p.token_id JOIN sender_identities i ON i.id=p.sender_identity_id JOIN smtp_accounts a ON a.id=i.smtp_account_id WHERE p.token_id=? AND p.sender_identity_id=? AND t.status='enabled' AND (t.expires_at IS NULL OR t.expires_at>?) AND i.enabled=TRUE AND i.deleted_at IS NULL AND a.enabled=TRUE AND a.deleted_at IS NULL").bind(token_id).bind(sender_identity_id).bind(Utc::now().naive_utc()).fetch_one(p).await.map(|n|n>0)}
}
