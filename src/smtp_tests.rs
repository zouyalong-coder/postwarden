use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use lettre::message::Mailbox;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    auth::AdminAuth,
    database::Database,
    http::AppState,
    smtp_transport::{
        self, Attachment, AuthMechanism, Message, SendOutcome, SmtpAuth, SmtpSettings, TlsMode,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/smtp-accounts/{id}/test-connection", post(test_connection))
        .route("/smtp-accounts/{id}/test-message", post(test_message))
}

#[derive(FromRow)]
struct AccountRow {
    id: String,
    host: String,
    port: i64,
    tls_mode: String,
    auth_type: String,
    username: Option<String>,
    credential_ciphertext: Option<Vec<u8>>,
}

#[derive(FromRow)]
struct MysqlAccountRow {
    id: String,
    host: String,
    port: u16,
    tls_mode: String,
    auth_type: String,
    username: Option<String>,
    credential_ciphertext: Option<Vec<u8>>,
}

impl From<MysqlAccountRow> for AccountRow {
    fn from(row: MysqlAccountRow) -> Self {
        Self {
            id: row.id,
            host: row.host,
            port: i64::from(row.port),
            tls_mode: row.tls_mode,
            auth_type: row.auth_type,
            username: row.username,
            credential_ciphertext: row.credential_ciphertext,
        }
    }
}

#[derive(Serialize)]
struct TestResult {
    success: bool,
    outcome: &'static str,
    message: &'static str,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
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

fn internal() -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Request could not be completed",
    )
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not_found", "SMTP account not found")
}

fn bad_request() -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Test message fields are invalid",
    )
}

fn map_db<T>(result: Result<T, sqlx::Error>) -> ApiResult<T> {
    result.map_err(|error| {
        tracing::error!(error = %error, "SMTP test account lookup failed");
        internal()
    })
}

async fn load_account(db: &Database, id: &str) -> ApiResult<AccountRow> {
    let row = match db {
        Database::Sqlite(pool) => map_db(
            sqlx::query_as::<_, AccountRow>(
                "SELECT id,host,port,tls_mode,auth_type,username,credential_ciphertext \
                 FROM smtp_accounts WHERE id=? AND deleted_at IS NULL",
            )
            .bind(id)
            .fetch_optional(pool)
            .await,
        )?,
        Database::Mysql(pool) => map_db(
            sqlx::query_as::<_, MysqlAccountRow>(
                "SELECT id,host,port,tls_mode,auth_type,username,credential_ciphertext \
                 FROM smtp_accounts WHERE id=? AND deleted_at IS NULL",
            )
            .bind(id)
            .fetch_optional(pool)
            .await,
        )?
        .map(Into::into),
    };
    row.ok_or_else(not_found)
}

fn settings(state: &AppState, row: &AccountRow) -> ApiResult<SmtpSettings> {
    let tls = match row.tls_mode.as_str() {
        "implicit" => TlsMode::Implicit,
        "starttls" => TlsMode::StartTls,
        "none" => TlsMode::None,
        _ => return Err(internal()),
    };
    let auth = match row.auth_type.as_str() {
        "none" => {
            if row.username.is_some() || row.credential_ciphertext.is_some() {
                return Err(internal());
            }
            None
        }
        "plain" | "login" => {
            let username = row.username.clone().ok_or_else(internal)?;
            let encrypted = row.credential_ciphertext.as_deref().ok_or_else(internal)?;
            let password = state.crypto.decrypt(encrypted).map_err(|_| internal())?;
            let password = String::from_utf8(password).map_err(|_| internal())?;
            Some(SmtpAuth {
                username,
                password,
                mechanism: if row.auth_type == "plain" {
                    AuthMechanism::Plain
                } else {
                    AuthMechanism::Login
                },
            })
        }
        _ => return Err(internal()),
    };
    let port = u16::try_from(row.port).map_err(|_| internal())?;
    Ok(SmtpSettings {
        host: row.host.clone(),
        port,
        tls,
        auth,
    })
}

async fn test_connection(
    admin: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<TestResult>> {
    let account = load_account(&state.database, &id).await?;
    let smtp_settings = settings(&state, &account)?;
    let success = smtp_transport::test_connection(&smtp_settings)
        .await
        .is_ok();
    persist_audit(
        &state.database,
        &admin.id,
        &account.id,
        "smtp_test_connection",
        if success {
            "connected"
        } else {
            "connection_failed"
        },
    )
    .await;
    Ok(Json(if success {
        TestResult {
            success: true,
            outcome: "connected",
            message: "SMTP connection succeeded",
        }
    } else {
        TestResult {
            success: false,
            outcome: "connection_failed",
            message: "SMTP connection failed",
        }
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestMessageRequest {
    /// The sender address used for this one test message.
    from: String,
    to: String,
}

async fn test_message(
    admin: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<TestMessageRequest>,
) -> ApiResult<Json<TestResult>> {
    let from = validate_address(&input.from)?;
    let to = validate_address(&input.to)?;
    let account = load_account(&state.database, &id).await?;
    let smtp_settings = settings(&state, &account)?;
    let message = Message {
        from,
        envelope_from: None,
        to: vec![to],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "PostWarden SMTP test".to_owned(),
        text: Some("This is a test message from PostWarden.".to_owned()),
        html: None,
        attachments: Vec::<Attachment>::new(),
    };
    let outcome = match smtp_transport::send(&smtp_settings, &message).await {
        Ok(SendOutcome::Accepted) => "accepted",
        Ok(SendOutcome::TransientRejection) => "transient_rejection",
        Ok(SendOutcome::PermanentRejection) => "permanent_rejection",
        Ok(SendOutcome::OutcomeUnknown) | Err(_) => "outcome_unknown",
    };
    // Keep the structured log aligned with the durable audit event while
    // excluding message contents and SMTP credentials.
    tracing::info!(
        target: "admin_audit",
        admin_id = %admin.id,
        smtp_account_id = %account.id,
        action = "smtp_test_message",
        outcome = outcome,
        "admin SMTP test message"
    );
    persist_audit(
        &state.database,
        &admin.id,
        &account.id,
        "smtp_test_message",
        outcome,
    )
    .await;
    Ok(Json(match outcome {
        "accepted" => TestResult {
            success: true,
            outcome: "accepted",
            message: "Test message was accepted by the SMTP server",
        },
        "transient_rejection" | "permanent_rejection" => TestResult {
            success: false,
            outcome,
            message: "SMTP server rejected the test message",
        },
        _ => TestResult {
            success: false,
            outcome: "outcome_unknown",
            message: "SMTP delivery outcome is unknown",
        },
    }))
}

async fn persist_audit(
    db: &Database,
    admin_id: &str,
    account_id: &str,
    action: &str,
    outcome: &str,
) {
    let id = Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now();
    let result: Result<(), sqlx::Error> = match db {
        Database::Sqlite(pool) => sqlx::query(
            "INSERT INTO admin_audit_events(id,admin_id,action,resource_id,outcome,created_at) VALUES(?,?,?,?,?,?)",
        )
        .bind(id)
        .bind(admin_id)
        .bind(action)
        .bind(account_id)
        .bind(outcome)
        .bind(created_at.to_rfc3339())
        .execute(pool)
        .await
        .map(|_| ()),
        Database::Mysql(pool) => sqlx::query(
            "INSERT INTO admin_audit_events(id,admin_id,action,resource_id,outcome,created_at) VALUES(?,?,?,?,?,?)",
        )
        .bind(id)
        .bind(admin_id)
        .bind(action)
        .bind(account_id)
        .bind(outcome)
        .bind(created_at.naive_utc())
        .execute(pool)
        .await
        .map(|_| ()),
    };
    if let Err(error) = result {
        // SMTP may already have accepted the message. Preserve that outcome
        // response so callers do not retry and create a duplicate.
        tracing::error!(error = %error, "could not persist admin SMTP test audit event");
    }
}

fn validate_address(value: &str) -> ApiResult<String> {
    if value.trim() != value || value.len() > 320 || value.contains(['\r', '\n']) {
        return Err(bad_request());
    }
    let mailbox = value.parse::<Mailbox>().map_err(|_| bad_request())?;
    Ok(mailbox.email.to_string())
}

#[cfg(test)]
mod tests {
    use super::validate_address;

    #[test]
    fn accepts_single_mailbox_and_rejects_header_injection() {
        assert_eq!(
            validate_address("person@example.test").unwrap(),
            "person@example.test"
        );
        assert!(validate_address("person@example.test\r\nBcc: hidden@example.test").is_err());
        assert!(validate_address("person@example.test,other@example.test").is_err());
    }
}
