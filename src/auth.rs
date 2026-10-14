use std::{collections::HashMap, error::Error};

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{FromRequestParts, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{Duration, NaiveDateTime, Utc};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{database::Database, http::AppState};

const SESSION_COOKIE: &str = "postwarden_session";
const CSRF_COOKIE: &str = "postwarden_csrf";
const SESSION_HOURS: i64 = 12;

/// The authenticated administrator identity attached to a protected handler.
#[derive(Clone, Debug)]
pub struct AdminAuth {
    pub id: String,
    pub login_name: String,
}

impl FromRequestParts<AppState> for AdminAuth {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let cookies = cookies(&parts.headers);
        let raw_session = cookies.get(SESSION_COOKIE).ok_or_else(unauthorized)?;
        if matches!(parts.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
            let raw_csrf = cookies.get(CSRF_COOKIE).ok_or_else(forbidden)?;
            let header_csrf = parts
                .headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(forbidden)?;
            if !constant_time_eq(raw_csrf.as_bytes(), header_csrf.as_bytes()) {
                return Err(forbidden());
            }
        }

        let digest = digest(raw_session.as_bytes());
        let current = Utc::now().naive_utc();
        let result = match &state.database {
            Database::Sqlite(pool) => {
                let now = sqlite_time(current);
                sqlx::query_as::<_, (String, String, String)>(
                    "SELECT s.id, a.id, a.login_name FROM admin_sessions s \
                     JOIN admin_users a ON a.id = s.admin_id \
                     WHERE s.session_digest = ? AND s.revoked_at IS NULL AND s.expires_at > ?",
                )
                .bind(digest.to_vec())
                .bind(now)
                .fetch_optional(pool)
                .await
            }
            Database::Mysql(pool) => {
                sqlx::query_as::<_, (String, String, String)>(
                    "SELECT s.id, a.id, a.login_name FROM admin_sessions s \
                     JOIN admin_users a ON a.id = s.admin_id \
                     WHERE s.session_digest = ? AND s.revoked_at IS NULL AND s.expires_at > ?",
                )
                .bind(digest.to_vec())
                .bind(current)
                .fetch_optional(pool)
                .await
            }
        }
        .map_err(|_| internal_error())?;
        let Some((_session_id, id, login_name)) = result else {
            return Err(unauthorized());
        };
        Ok(Self { id, login_name })
    }
}

/// Router mounted beneath `/api/v1/admin` by the application router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/session", get(get_session).post(login).delete(logout))
        .route("/password", post(change_password))
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    current_password: String,
    new_password: String,
}

async fn change_password(
    admin: AdminAuth,
    State(state): State<AppState>,
    Json(request): Json<ChangePasswordRequest>,
) -> Response {
    if request.new_password.chars().count() < 12 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_password",
            "New password must be at least 12 characters",
        );
    }
    if request.current_password == request.new_password {
        return api_error(
            StatusCode::BAD_REQUEST,
            "password_unchanged",
            "New password must differ from current password",
        );
    }
    let Some((_, _, old_hash)) = (match find_admin(&state.database, &admin.login_name).await {
        Ok(value) => value,
        Err(()) => return internal_error(),
    }) else {
        return unauthorized();
    };
    let parsed = match PasswordHash::new(&old_hash) {
        Ok(hash) => hash,
        Err(_) => return internal_error(),
    };
    if Argon2::default()
        .verify_password(request.current_password.as_bytes(), &parsed)
        .is_err()
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_current_password",
            "Current password is incorrect",
        );
    }
    let new_hash = match hash_password(&request.new_password) {
        Ok(hash) => hash,
        Err(_) => return internal_error(),
    };
    match replace_password(&state.database, &admin.id, Some(&old_hash), &new_hash).await {
        Ok(true) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            append_cookie(
                &mut response,
                &expired_cookie(SESSION_COOKIE, state.cookie_secure, true),
            );
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Ok(false) => api_error(
            StatusCode::CONFLICT,
            "password_changed",
            "Password changed; sign in again",
        ),
        Err(_) => internal_error(),
    }
}

/// Reset the singleton administrator's password during an explicitly requested startup.
pub async fn reset_admin_password(
    database: &Database,
    password: &str,
) -> Result<(), Box<dyn Error>> {
    if password.chars().count() < 12 {
        return Err("administrator password must be at least 12 characters".into());
    }
    let id: Option<String> = match database {
        Database::Sqlite(pool) => {
            sqlx::query_scalar("SELECT id FROM admin_users")
                .fetch_optional(pool)
                .await?
        }
        Database::Mysql(pool) => {
            sqlx::query_scalar("SELECT id FROM admin_users")
                .fetch_optional(pool)
                .await?
        }
    };
    let id = id.ok_or("administrator has not been initialized")?;
    let hash = hash_password(password).map_err(|_| "could not hash administrator password")?;
    if !replace_password(database, &id, None, &hash).await? {
        return Err("administrator no longer exists".into());
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string())
}

async fn replace_password(
    database: &Database,
    id: &str,
    expected_hash: Option<&str>,
    new_hash: &str,
) -> Result<bool, sqlx::Error> {
    let now = Utc::now().naive_utc();
    match database {
        Database::Sqlite(pool) => {
            let mut tx = pool.begin().await?;
            let mut query = if expected_hash.is_some() {
                sqlx::query("UPDATE admin_users SET password_hash=?, updated_at=? WHERE id=? AND password_hash=?")
            } else {
                sqlx::query("UPDATE admin_users SET password_hash=?, updated_at=? WHERE id=?")
            };
            query = query.bind(new_hash).bind(sqlite_time(now)).bind(id);
            if let Some(hash) = expected_hash {
                query = query.bind(hash);
            }
            let changed = query.execute(&mut *tx).await?.rows_affected() == 1;
            if changed {
                sqlx::query("UPDATE admin_sessions SET revoked_at=? WHERE admin_id=? AND revoked_at IS NULL")
                    .bind(sqlite_time(now)).bind(id).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok(changed)
        }
        Database::Mysql(pool) => {
            let mut tx = pool.begin().await?;
            let mut query = if expected_hash.is_some() {
                sqlx::query("UPDATE admin_users SET password_hash=?, updated_at=? WHERE id=? AND password_hash=?")
            } else {
                sqlx::query("UPDATE admin_users SET password_hash=?, updated_at=? WHERE id=?")
            };
            query = query.bind(new_hash).bind(now).bind(id);
            if let Some(hash) = expected_hash {
                query = query.bind(hash);
            }
            let changed = query.execute(&mut *tx).await?.rows_affected() == 1;
            if changed {
                sqlx::query("UPDATE admin_sessions SET revoked_at=? WHERE admin_id=? AND revoked_at IS NULL")
                    .bind(now).bind(id).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok(changed)
        }
    }
}

/// Create the initial administrator once.
pub async fn initialize_admin(
    database: &Database,
    login_name: &str,
    password: &str,
) -> Result<(), Box<dyn Error>> {
    if login_name.trim().is_empty() || password.chars().count() < 12 {
        return Err(
            "administrator login must be non-empty and password at least 12 characters".into(),
        );
    }
    let exists = match database {
        Database::Sqlite(pool) => {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM admin_users")
                .fetch_one(pool)
                .await?
                > 0
        }
        Database::Mysql(pool) => {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM admin_users")
                .fetch_one(pool)
                .await?
                > 0
        }
    };
    if exists {
        return Err("administrator already exists".into());
    }

    let password_hash =
        hash_password(password).map_err(|_| "could not hash administrator password")?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().naive_utc();
    match database {
        Database::Sqlite(pool) => {
            sqlx::query("INSERT INTO admin_users (id, login_name, password_hash, created_at, updated_at) VALUES (?, ?, ?, ?, ?)")
                .bind(id)
                .bind(login_name.trim())
                .bind(password_hash)
                .bind(sqlite_time(now))
                .bind(sqlite_time(now))
                .execute(pool)
                .await?;
        }
        Database::Mysql(pool) => {
            sqlx::query("INSERT INTO admin_users (id, login_name, password_hash, created_at, updated_at) VALUES (?, ?, ?, ?, ?)")
                .bind(id)
                .bind(login_name.trim())
                .bind(password_hash)
                .bind(now)
                .bind(now)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

/// Whether the singleton administrator has already been initialized.
pub async fn has_admin(database: &Database) -> Result<bool, sqlx::Error> {
    let count: i64 = match database {
        Database::Sqlite(pool) => {
            sqlx::query_scalar("SELECT COUNT(*) FROM admin_users")
                .fetch_one(pool)
                .await?
        }
        Database::Mysql(pool) => {
            sqlx::query_scalar("SELECT COUNT(*) FROM admin_users")
                .fetch_one(pool)
                .await?
        }
    };
    Ok(count > 0)
}

#[derive(Deserialize)]
struct LoginRequest {
    login_name: String,
    password: String,
}

#[derive(Serialize)]
struct SessionResponse {
    authenticated: bool,
    login_name: Option<String>,
}

async fn get_session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let mut response = if let Some(raw) = cookies(&headers).get(SESSION_COOKIE) {
        match lookup_session(&state.database, raw).await {
            Ok(Some(login_name)) => Json(SessionResponse {
                authenticated: true,
                login_name: Some(login_name),
            })
            .into_response(),
            Ok(None) => Json(SessionResponse {
                authenticated: false,
                login_name: None,
            })
            .into_response(),
            Err(()) => return internal_error(),
        }
    } else {
        Json(SessionResponse {
            authenticated: false,
            login_name: None,
        })
        .into_response()
    };
    if cookies(&headers).get(CSRF_COOKIE).is_none() {
        let token = random_token(32);
        append_cookie(&mut response, &csrf_cookie(&token, state.cookie_secure));
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Response {
    if !valid_csrf(&headers) {
        return forbidden();
    }
    let admin = match find_admin(&state.database, &request.login_name).await {
        Ok(admin) => admin,
        Err(()) => return internal_error(),
    };
    let Some((admin_id, login_name, encoded_hash)) = admin else {
        // Keep the missing-user and wrong-password paths indistinguishable.
        let _ = Argon2::default().verify_password(
            b"invalid password",
            &PasswordHash::new("$argon2id$v=19$m=19456,t=2,p=1$YWJjZGVmZ2hpamts$YWJjZGVmZ2hpamtsYWJjZGVmZ2hpamtsYWJjZGVmZ2hpamts").unwrap(),
        );
        return unauthorized();
    };
    let parsed = match PasswordHash::new(&encoded_hash) {
        Ok(hash) => hash,
        Err(_) => return internal_error(),
    };
    if Argon2::default()
        .verify_password(request.password.as_bytes(), &parsed)
        .is_err()
    {
        return unauthorized();
    }

    let raw_session = random_token(32);
    let digest = digest(raw_session.as_bytes());
    let now = Utc::now().naive_utc();
    let expiry = now + Duration::hours(SESSION_HOURS);
    let session_id = Uuid::new_v4().to_string();
    let result = match &state.database {
        Database::Sqlite(pool) => sqlx::query(
            "INSERT INTO admin_sessions (id, admin_id, session_digest, expires_at, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(admin_id)
        .bind(digest.to_vec())
        .bind(sqlite_time(expiry))
        .bind(sqlite_time(now))
        .execute(pool)
        .await
        .map(|_| ()),
        Database::Mysql(pool) => sqlx::query(
            "INSERT INTO admin_sessions (id, admin_id, session_digest, expires_at, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(admin_id)
        .bind(digest.to_vec())
        .bind(expiry)
        .bind(now)
        .execute(pool)
        .await
        .map(|_| ()),
    };
    if result.is_err() {
        return internal_error();
    }

    let mut response = Json(SessionResponse {
        authenticated: true,
        login_name: Some(login_name),
    })
    .into_response();
    append_cookie(
        &mut response,
        &session_cookie(&raw_session, state.cookie_secure),
    );
    // Rotate the pre-authentication CSRF value after authentication.
    append_cookie(
        &mut response,
        &csrf_cookie(&random_token(32), state.cookie_secure),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !valid_csrf(&headers) {
        return forbidden();
    }
    if let Some(raw) = cookies(&headers).get(SESSION_COOKIE) {
        let digest = digest(raw.as_bytes());
        let now = Utc::now().naive_utc();
        let result = match &state.database {
            Database::Sqlite(pool) => sqlx::query(
                "UPDATE admin_sessions SET revoked_at = ? WHERE session_digest = ? AND revoked_at IS NULL",
            )
            .bind(sqlite_time(now))
            .bind(digest.to_vec())
            .execute(pool)
            .await
            .map(|_| ()),
            Database::Mysql(pool) => sqlx::query(
                "UPDATE admin_sessions SET revoked_at = ? WHERE session_digest = ? AND revoked_at IS NULL",
            )
            .bind(now)
            .bind(digest.to_vec())
            .execute(pool)
            .await
            .map(|_| ()),
        };
        if result.is_err() {
            return internal_error();
        }
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    append_cookie(
        &mut response,
        &expired_cookie(SESSION_COOKIE, state.cookie_secure, true),
    );
    append_cookie(
        &mut response,
        &expired_cookie(CSRF_COOKIE, state.cookie_secure, false),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn find_admin(
    database: &Database,
    name: &str,
) -> Result<Option<(String, String, String)>, ()> {
    match database {
        Database::Sqlite(pool) => sqlx::query_as(
            "SELECT id, login_name, password_hash FROM admin_users WHERE login_name = ?",
        )
        .bind(name)
        .fetch_optional(pool)
        .await
        .map_err(|_| ()),
        Database::Mysql(pool) => sqlx::query_as(
            "SELECT id, login_name, password_hash FROM admin_users WHERE login_name = ?",
        )
        .bind(name)
        .fetch_optional(pool)
        .await
        .map_err(|_| ()),
    }
}

async fn lookup_session(database: &Database, raw: &str) -> Result<Option<String>, ()> {
    let digest = digest(raw.as_bytes());
    let now = Utc::now().naive_utc();
    match database {
        Database::Sqlite(pool) => sqlx::query_scalar(
            "SELECT a.login_name FROM admin_sessions s JOIN admin_users a ON a.id = s.admin_id \
             WHERE s.session_digest = ? AND s.revoked_at IS NULL AND s.expires_at > ?",
        )
        .bind(digest.to_vec())
        .bind(sqlite_time(now))
        .fetch_optional(pool)
        .await
        .map_err(|_| ()),
        Database::Mysql(pool) => sqlx::query_scalar(
            "SELECT a.login_name FROM admin_sessions s JOIN admin_users a ON a.id = s.admin_id \
             WHERE s.session_digest = ? AND s.revoked_at IS NULL AND s.expires_at > ?",
        )
        .bind(digest.to_vec())
        .bind(now)
        .fetch_optional(pool)
        .await
        .map_err(|_| ()),
    }
}

fn valid_csrf(headers: &HeaderMap) -> bool {
    let cookies = cookies(headers);
    match (
        cookies.get(CSRF_COOKIE),
        headers.get("x-csrf-token").and_then(|v| v.to_str().ok()),
    ) {
        (Some(cookie), Some(value)) => constant_time_eq(cookie.as_bytes(), value.as_bytes()),
        _ => false,
    }
}

fn cookies(headers: &HeaderMap) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for header in headers.get_all(header::COOKIE) {
        if let Ok(value) = header.to_str() {
            for part in value.split(';') {
                if let Some((key, value)) = part.trim().split_once('=') {
                    values.insert(key.to_owned(), value.to_owned());
                }
            }
        }
    }
    values
}

fn random_token(bytes: usize) -> String {
    let mut value = vec![0; bytes];
    OsRng.fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn sqlite_time(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

fn session_cookie(value: &str, secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={};{}",
        SESSION_HOURS * 3600,
        secure_suffix(secure)
    )
}

fn csrf_cookie(value: &str, secure: bool) -> String {
    format!(
        "{CSRF_COOKIE}={value}; Path=/; SameSite=Strict; Max-Age={};{}",
        SESSION_HOURS * 3600,
        secure_suffix(secure)
    )
}

fn expired_cookie(name: &str, secure: bool, http_only: bool) -> String {
    format!(
        "{name}=; Path=/; SameSite=Strict; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT;{}{}",
        if http_only { " HttpOnly;" } else { "" },
        secure_suffix(secure)
    )
}

fn secure_suffix(secure: bool) -> &'static str {
    if secure {
        " Secure"
    } else {
        ""
    }
}

fn append_cookie(response: &mut Response, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

fn unauthorized() -> Response {
    api_error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "Authentication required",
    )
}

fn forbidden() -> Response {
    api_error(
        StatusCode::FORBIDDEN,
        "csrf_failed",
        "CSRF validation failed",
    )
}

fn internal_error() -> Response {
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Internal server error",
    )
}

#[derive(Serialize)]
struct ApiError {
    code: &'static str,
    message: &'static str,
}

fn api_error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (status, Json(ApiError { code, message })).into_response()
}
