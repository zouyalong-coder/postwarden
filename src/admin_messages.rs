use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::{auth::AdminAuth, database::Database, http::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/messages", get(list))
        .route("/messages/{id}", get(detail))
}

#[derive(Deserialize)]
struct Filter {
    token_id: Option<String>,
    sender_identity_id: Option<String>,
    status: Option<String>,
    limit: Option<i64>,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

#[derive(FromRow)]
struct SqliteMessage {
    id: String,
    token_id: String,
    sender_identity_id: String,
    identity_version: i64,
    mode: String,
    status: String,
    created_at: String,
    queued_at: Option<String>,
    finished_at: Option<String>,
    next_attempt_at: Option<String>,
    last_error_code: Option<String>,
}

#[derive(FromRow)]
struct MysqlMessage {
    id: String,
    token_id: String,
    sender_identity_id: String,
    identity_version: u64,
    mode: String,
    status: String,
    created_at: NaiveDateTime,
    queued_at: Option<NaiveDateTime>,
    finished_at: Option<NaiveDateTime>,
    next_attempt_at: Option<NaiveDateTime>,
    last_error_code: Option<String>,
}

#[derive(Serialize)]
struct MessageView {
    id: String,
    token_id: String,
    sender_identity_id: String,
    identity_version: i64,
    mode: String,
    status: String,
    created_at: String,
    queued_at: Option<String>,
    finished_at: Option<String>,
    next_attempt_at: Option<String>,
    last_error_code: Option<String>,
}

fn timestamp(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}
impl From<SqliteMessage> for MessageView {
    fn from(r: SqliteMessage) -> Self {
        Self {
            id: r.id,
            token_id: r.token_id,
            sender_identity_id: r.sender_identity_id,
            identity_version: r.identity_version,
            mode: r.mode,
            status: r.status,
            created_at: r.created_at,
            queued_at: r.queued_at,
            finished_at: r.finished_at,
            next_attempt_at: r.next_attempt_at,
            last_error_code: r.last_error_code,
        }
    }
}
impl From<MysqlMessage> for MessageView {
    fn from(r: MysqlMessage) -> Self {
        Self {
            id: r.id,
            token_id: r.token_id,
            sender_identity_id: r.sender_identity_id,
            identity_version: r.identity_version as i64,
            mode: r.mode,
            status: r.status,
            created_at: timestamp(r.created_at),
            queued_at: r.queued_at.map(timestamp),
            finished_at: r.finished_at.map(timestamp),
            next_attempt_at: r.next_attempt_at.map(timestamp),
            last_error_code: r.last_error_code,
        }
    }
}

type ApiError = (StatusCode, Json<ErrorBody>);
fn internal(error: sqlx::Error) -> ApiError {
    tracing::error!(error=%error, "admin message query failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            code: "internal_error",
            message: "Request could not be completed",
        }),
    )
}
fn missing() -> ApiError {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            code: "not_found",
            message: "Message not found",
        }),
    )
}

async fn list(
    _: AdminAuth,
    State(state): State<AppState>,
    Query(filter): Query<Filter>,
) -> Result<Json<Vec<MessageView>>, ApiError> {
    let limit = filter.limit.unwrap_or(50).clamp(1, 100);
    let sql="SELECT id,token_id,sender_identity_id,identity_version,mode,status,created_at,queued_at,finished_at,next_attempt_at,last_error_code FROM messages WHERE (? IS NULL OR token_id=?) AND (? IS NULL OR sender_identity_id=?) AND (? IS NULL OR status=?) ORDER BY created_at DESC LIMIT ?";
    let rows = match &state.database {
        Database::Sqlite(pool) => sqlx::query_as::<_, SqliteMessage>(sql)
            .bind(&filter.token_id)
            .bind(&filter.token_id)
            .bind(&filter.sender_identity_id)
            .bind(&filter.sender_identity_id)
            .bind(&filter.status)
            .bind(&filter.status)
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(internal)?
            .into_iter()
            .map(Into::into)
            .collect(),
        Database::Mysql(pool) => sqlx::query_as::<_, MysqlMessage>(sql)
            .bind(&filter.token_id)
            .bind(&filter.token_id)
            .bind(&filter.sender_identity_id)
            .bind(&filter.sender_identity_id)
            .bind(&filter.status)
            .bind(&filter.status)
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(internal)?
            .into_iter()
            .map(Into::into)
            .collect(),
    };
    Ok(Json(rows))
}

#[derive(FromRow, Serialize)]
struct Attempt {
    id: String,
    attempt_no: i64,
    result: Option<String>,
    smtp_stage: Option<String>,
    safe_error_code: Option<String>,
}
#[derive(FromRow)]
struct MysqlAttempt {
    id: String,
    attempt_no: u32,
    result: Option<String>,
    smtp_stage: Option<String>,
    safe_error_code: Option<String>,
}
impl From<MysqlAttempt> for Attempt {
    fn from(r: MysqlAttempt) -> Self {
        Self {
            id: r.id,
            attempt_no: i64::from(r.attempt_no),
            result: r.result,
            smtp_stage: r.smtp_stage,
            safe_error_code: r.safe_error_code,
        }
    }
}
#[derive(FromRow, Serialize)]
struct Event {
    id: String,
    event_type: String,
    plugin_id: Option<String>,
    safe_details: Option<String>,
}
#[derive(Serialize)]
struct Detail {
    #[serde(flatten)]
    message: MessageView,
    recipients: serde_json::Value,
    attempts: Vec<Attempt>,
    events: Vec<Event>,
}
async fn detail(
    _: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Detail>, ApiError> {
    let sql="SELECT id,token_id,sender_identity_id,identity_version,mode,status,created_at,queued_at,finished_at,next_attempt_at,last_error_code FROM messages WHERE id=?";
    let message = match &state.database {
        Database::Sqlite(pool) => sqlx::query_as::<_, SqliteMessage>(sql)
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(internal)?
            .map(Into::into),
        Database::Mysql(pool) => sqlx::query_as::<_, MysqlMessage>(sql)
            .bind(&id)
            .fetch_optional(pool)
            .await
            .map_err(internal)?
            .map(Into::into),
    }
    .ok_or_else(missing)?;
    let recipient_ciphertext = match &state.database {
        Database::Sqlite(pool) => {
            sqlx::query_scalar::<_, Vec<u8>>("SELECT recipient_ciphertext FROM messages WHERE id=?")
                .bind(&id)
                .fetch_one(pool)
                .await
                .map_err(internal)?
        }
        Database::Mysql(pool) => {
            sqlx::query_scalar::<_, Vec<u8>>("SELECT recipient_ciphertext FROM messages WHERE id=?")
                .bind(&id)
                .fetch_one(pool)
                .await
                .map_err(internal)?
        }
    };
    let recipient_bytes = state.crypto.decrypt(&recipient_ciphertext).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                code: "internal_error",
                message: "Request could not be completed",
            }),
        )
    })?;
    let recipients = serde_json::from_slice(&recipient_bytes).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                code: "internal_error",
                message: "Request could not be completed",
            }),
        )
    })?;
    let (attempts,events)=match &state.database {
        Database::Sqlite(pool)=>(sqlx::query_as::<_,Attempt>("SELECT id,attempt_no,result,smtp_stage,safe_error_code FROM message_attempts WHERE message_id=? ORDER BY attempt_no").bind(&id).fetch_all(pool).await.map_err(internal)?,sqlx::query_as::<_,Event>("SELECT id,event_type,plugin_id,safe_details FROM message_events WHERE message_id=? ORDER BY created_at").bind(&id).fetch_all(pool).await.map_err(internal)?),
        Database::Mysql(pool)=>(sqlx::query_as::<_,MysqlAttempt>("SELECT id,attempt_no,result,smtp_stage,safe_error_code FROM message_attempts WHERE message_id=? ORDER BY attempt_no").bind(&id).fetch_all(pool).await.map_err(internal)?.into_iter().map(Into::into).collect(),sqlx::query_as::<_,Event>("SELECT id,event_type,plugin_id,safe_details FROM message_events WHERE message_id=? ORDER BY created_at").bind(&id).fetch_all(pool).await.map_err(internal)?),
    };
    Ok(Json(Detail {
        message,
        recipients,
        attempts,
        events,
    }))
}
