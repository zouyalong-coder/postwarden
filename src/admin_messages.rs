use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{Duration, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{auth::AdminAuth, database::Database, http::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/messages", get(list))
        .route("/messages/{id}", get(detail))
        .route("/messages/{id}/resume", post(resume_held))
        .route("/messages/{id}/reject", post(reject_held))
}

#[derive(Deserialize)]
struct Filter {
    token_id: Option<String>,
    sender_identity_id: Option<String>,
    status: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Serialize)]
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

fn not_held() -> ApiError {
    (
        StatusCode::CONFLICT,
        Json(ErrorBody {
            code: "message_not_held",
            message: "Message is not awaiting plugin action",
        }),
    )
}

#[derive(Serialize)]
struct ActionResult {
    message_id: String,
    status: &'static str,
}

async fn resume_held(
    admin: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, ApiError> {
    let now = Utc::now();
    let changed = match &state.database {
        Database::Sqlite(pool) => {
            let mut tx = pool.begin().await.map_err(internal)?;
            let changed=sqlx::query("UPDATE plugin_message_states SET resume_at=? WHERE message_id=? AND EXISTS (SELECT 1 FROM messages WHERE id=? AND status='held')")
                .bind(now.to_rfc3339()).bind(&id).bind(&id).execute(&mut *tx).await.map_err(internal)?.rows_affected();
            if changed > 0 {
                sqlx::query("INSERT INTO message_events(id,message_id,event_type,created_at,safe_details) VALUES(?,?,'admin_resume_requested',?,NULL)")
                    .bind(Uuid::new_v4().to_string()).bind(&id).bind(now.to_rfc3339()).execute(&mut *tx).await.map_err(internal)?;
                insert_admin_audit_sqlite(
                    &mut tx,
                    &admin.id,
                    &id,
                    "message_resume",
                    "requested",
                    now,
                )
                .await?;
            }
            tx.commit().await.map_err(internal)?;
            changed
        }
        Database::Mysql(pool) => {
            let mut tx = pool.begin().await.map_err(internal)?;
            let changed=sqlx::query("UPDATE plugin_message_states SET resume_at=? WHERE message_id=? AND EXISTS (SELECT 1 FROM messages WHERE id=? AND status='held')")
                .bind(now.naive_utc()).bind(&id).bind(&id).execute(&mut *tx).await.map_err(internal)?.rows_affected();
            if changed > 0 {
                sqlx::query("INSERT INTO message_events(id,message_id,event_type,created_at,safe_details) VALUES(?,?,'admin_resume_requested',?,NULL)")
                    .bind(Uuid::new_v4().to_string()).bind(&id).bind(now.naive_utc()).execute(&mut *tx).await.map_err(internal)?;
                insert_admin_audit_mysql(
                    &mut tx,
                    &admin.id,
                    &id,
                    "message_resume",
                    "requested",
                    now,
                )
                .await?;
            }
            tx.commit().await.map_err(internal)?;
            changed
        }
    };
    if changed == 0 {
        return Err(not_held());
    }
    Ok(Json(ActionResult {
        message_id: id,
        status: "held",
    }))
}

async fn reject_held(
    admin: AdminAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, ApiError> {
    let now = Utc::now();
    let content = now + Duration::days(state.retention.content_days);
    let audit = now + Duration::days(state.retention.audit_days);
    let changed = match &state.database {
        Database::Sqlite(pool) => {
            let mut tx = pool.begin().await.map_err(internal)?;
            let changed=sqlx::query("UPDATE messages SET status='rejected',finished_at=?,content_retained_until=?,audit_retained_until=?,last_error_code='admin_rejected' WHERE id=? AND status='held'")
                .bind(now.to_rfc3339()).bind(content.to_rfc3339()).bind(audit.to_rfc3339()).bind(&id).execute(&mut *tx).await.map_err(internal)?.rows_affected();
            if changed > 0 {
                sqlx::query("UPDATE message_payloads SET retained_until=? WHERE message_id=?")
                    .bind(content.to_rfc3339())
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .map_err(internal)?;
                sqlx::query("INSERT INTO message_events(id,message_id,event_type,created_at,safe_details) VALUES(?,?,'admin_rejected',?,NULL)").bind(Uuid::new_v4().to_string()).bind(&id).bind(now.to_rfc3339()).execute(&mut *tx).await.map_err(internal)?;
                insert_admin_audit_sqlite(
                    &mut tx,
                    &admin.id,
                    &id,
                    "message_reject",
                    "rejected",
                    now,
                )
                .await?;
            }
            tx.commit().await.map_err(internal)?;
            changed
        }
        Database::Mysql(pool) => {
            let mut tx = pool.begin().await.map_err(internal)?;
            let changed=sqlx::query("UPDATE messages SET status='rejected',finished_at=?,content_retained_until=?,audit_retained_until=?,last_error_code='admin_rejected' WHERE id=? AND status='held'")
                .bind(now.naive_utc()).bind(content.naive_utc()).bind(audit.naive_utc()).bind(&id).execute(&mut *tx).await.map_err(internal)?.rows_affected();
            if changed > 0 {
                sqlx::query("UPDATE message_payloads SET retained_until=? WHERE message_id=?")
                    .bind(content.naive_utc())
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .map_err(internal)?;
                sqlx::query("INSERT INTO message_events(id,message_id,event_type,created_at,safe_details) VALUES(?,?,'admin_rejected',?,NULL)").bind(Uuid::new_v4().to_string()).bind(&id).bind(now.naive_utc()).execute(&mut *tx).await.map_err(internal)?;
                insert_admin_audit_mysql(
                    &mut tx,
                    &admin.id,
                    &id,
                    "message_reject",
                    "rejected",
                    now,
                )
                .await?;
            }
            tx.commit().await.map_err(internal)?;
            changed
        }
    };
    if changed == 0 {
        return Err(not_held());
    }
    Ok(Json(ActionResult {
        message_id: id,
        status: "rejected",
    }))
}

async fn insert_admin_audit_sqlite(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    admin: &str,
    id: &str,
    action: &str,
    outcome: &str,
    now: chrono::DateTime<Utc>,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO admin_audit_events(id,admin_id,action,resource_id,outcome,created_at) VALUES(?,?,?,?,?,?)")
        .bind(Uuid::new_v4().to_string()).bind(admin).bind(action).bind(id).bind(outcome).bind(now.to_rfc3339()).execute(&mut **tx).await.map_err(internal)?;
    Ok(())
}
async fn insert_admin_audit_mysql(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    admin: &str,
    id: &str,
    action: &str,
    outcome: &str,
    now: chrono::DateTime<Utc>,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO admin_audit_events(id,admin_id,action,resource_id,outcome,created_at) VALUES(?,?,?,?,?,?)")
        .bind(Uuid::new_v4().to_string()).bind(admin).bind(action).bind(id).bind(outcome).bind(now.naive_utc()).execute(&mut **tx).await.map_err(internal)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use axum::extract::{Path, State};
    use chrono::Utc;

    use super::{reject_held, resume_held};
    use crate::{
        auth::{self, AdminAuth},
        config::{Config, DatabaseKind, RetentionPolicy},
        crypto::Crypto,
        database::Database,
        http::AppState,
        plugins::PluginManager,
    };

    #[tokio::test]
    async fn admin_can_resume_or_reject_held_message() {
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
        auth::initialize_admin(&db, "admin", "TestPassword123!")
            .await
            .unwrap();
        let Database::Sqlite(pool) = &db else {
            unreachable!()
        };
        let admin_id: String = sqlx::query_scalar("SELECT id FROM admin_users")
            .fetch_one(pool)
            .await
            .unwrap();
        let now = Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO smtp_accounts(id,name,host,port,tls_mode,auth_type,enabled,created_at,updated_at) VALUES('a','test','localhost',25,'none','none',1,?,?)").bind(&now).bind(&now).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO sender_identities(id,smtp_account_id,display_name,email_address,envelope_from,enabled,created_at,updated_at) VALUES('i','a','Test','test@example.com','test@example.com',1,?,?)").bind(&now).bind(&now).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO api_tokens(id,name,lookup_prefix,secret_digest,status,created_at) VALUES('t','test','prefix',?,'enabled',?)").bind(vec![1_u8;32]).bind(&now).execute(pool).await.unwrap();
        let crypto = Crypto::new([5; 32]);
        let recipient_cipher = crypto
            .encrypt(br#"{"to":["x@example.com"],"cc":[],"bcc":[]}"#)
            .unwrap();
        let state = AppState {
            database: db.clone(),
            crypto: crypto.clone(),
            cookie_secure: false,
            plugins: PluginManager::new(db.clone(), crypto).await.unwrap(),
            retention: config.retention,
        };
        let ids = [
            "msg_11111111111111111111111111111111",
            "msg_22222222222222222222222222222222",
        ];
        for (index, id) in ids.iter().enumerate() {
            sqlx::query("INSERT INTO messages(id,token_id,sender_identity_id,identity_version,mode,status,idempotency_key,request_digest,recipient_ciphertext,created_at) VALUES(?,'t','i',1,'async','held',?,?,?,?)")
                .bind(id).bind(format!("key{index}")).bind(vec![index as u8;32]).bind(&recipient_cipher).bind(&now).execute(pool).await.unwrap();
            sqlx::query("INSERT INTO message_payloads(message_id,payload_ciphertext,key_version) VALUES(?,?,1)").bind(id).bind(vec![3_u8;32]).execute(pool).await.unwrap();
            sqlx::query("INSERT INTO plugin_message_states(message_id,plugin_id,plugin_version,state_ciphertext) VALUES(?,'example-policy','1',?)").bind(id).bind(vec![4_u8;32]).execute(pool).await.unwrap();
        }
        let admin = AdminAuth {
            id: admin_id,
            login_name: "admin".into(),
        };
        let _ = resume_held(admin.clone(), State(state.clone()), Path(ids[0].into()))
            .await
            .unwrap();
        let resume_at: Option<String> =
            sqlx::query_scalar("SELECT resume_at FROM plugin_message_states WHERE message_id=?")
                .bind(ids[0])
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(resume_at.is_some());
        let _ = reject_held(admin, State(state), Path(ids[1].into()))
            .await
            .unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM messages WHERE id=?")
            .bind(ids[1])
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(status, "rejected");
        let retention: Option<String> =
            sqlx::query_scalar("SELECT retained_until FROM message_payloads WHERE message_id=?")
                .bind(ids[1])
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(retention.is_some());
        let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admin_audit_events")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(audits, 2);
    }
}
