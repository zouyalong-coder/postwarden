use chrono::{Duration, NaiveDateTime, Utc};
use tokio::{
    task::JoinHandle,
    time::{interval, Duration as TokioDuration},
};

use crate::{database::Database, http::AppState};

const POLL_SECONDS: u64 = 2;
const CLAIM_BATCH: i64 = 32;

/// Start the single in-process queue worker. It reconciles interrupted work before polling.
pub fn spawn(state: AppState) -> JoinHandle<()> {
    tokio::spawn(run(state))
}

pub async fn run(state: AppState) {
    if let Err(error) = mark_interrupted_unknown(&state.database, &state).await {
        tracing::error!(error = %error, "could not reconcile interrupted messages");
        return;
    }
    let mut ticker = interval(TokioDuration::from_secs(POLL_SECONDS));
    loop {
        ticker.tick().await;
        if let Err(error) = expire_pending(&state.database, &state).await {
            tracing::error!(error = %error, "could not expire pending messages");
        }
        if let Err(error) = clean_retained_data(&state.database, &state).await {
            tracing::error!(error = %error, "could not clean retained message data");
        }
        for id in match claim_batch(&state.database).await {
            Ok(ids) => ids,
            Err(error) => {
                tracing::error!(error = %error, "could not claim queued messages");
                continue;
            }
        } {
            if let Err(error) = crate::messages::process_async(&state, &id).await {
                tracing::error!(message_id = %id, error = %error, "message worker failed");
                if let Err(db_error) = mark_worker_failure(&state.database, &state, &id).await {
                    tracing::error!(message_id = %id, error = %db_error, "could not record worker failure");
                }
            }
        }
    }
}

fn retention_deadlines(
    state: &AppState,
    now: chrono::DateTime<Utc>,
) -> (String, String, NaiveDateTime, NaiveDateTime) {
    let content = now + Duration::days(state.retention.content_days.max(0));
    let audit = now + Duration::days(state.retention.audit_days.max(0));
    (
        content.to_rfc3339(),
        audit.to_rfc3339(),
        content.naive_utc(),
        audit.naive_utc(),
    )
}

async fn mark_interrupted_unknown(db: &Database, state: &AppState) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    let (content, audit, mysql_content, mysql_audit) = retention_deadlines(state, now);
    match db {
        Database::Sqlite(pool) => {
            sqlx::query("UPDATE message_attempts SET finished_at=?,result='unknown',safe_error_code='worker_interrupted' WHERE finished_at IS NULL AND message_id IN (SELECT id FROM messages WHERE status='processing')").bind(now.to_rfc3339()).execute(pool).await?;
            sqlx::query("UPDATE messages SET status='outcome_unknown',finished_at=?,content_retained_until=COALESCE(content_retained_until,?),audit_retained_until=COALESCE(audit_retained_until,?),last_error_code='worker_interrupted' WHERE status='processing'").bind(now.to_rfc3339()).bind(content).bind(audit).execute(pool).await?;
        }
        Database::Mysql(pool) => {
            sqlx::query("UPDATE message_attempts SET finished_at=?,result='unknown',safe_error_code='worker_interrupted' WHERE finished_at IS NULL AND message_id IN (SELECT id FROM messages WHERE status='processing')").bind(now.naive_utc()).execute(pool).await?;
            sqlx::query("UPDATE messages SET status='outcome_unknown',finished_at=?,content_retained_until=COALESCE(content_retained_until,?),audit_retained_until=COALESCE(audit_retained_until,?),last_error_code='worker_interrupted' WHERE status='processing'").bind(now.naive_utc()).bind(mysql_content).bind(mysql_audit).execute(pool).await?;
        }
    }
    Ok(())
}

async fn expire_pending(db: &Database, state: &AppState) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    let pending_cutoff = now - Duration::days(state.retention.pending_days.max(0));
    let (content, audit, mysql_content, mysql_audit) = retention_deadlines(state, now);
    match db {
        Database::Sqlite(pool) => {
            sqlx::query("UPDATE messages SET status='expired',finished_at=?,content_retained_until=COALESCE(content_retained_until,?),audit_retained_until=COALESCE(audit_retained_until,?),last_error_code='message_expired' WHERE status IN ('queued','retry_wait','held') AND ((expires_at IS NOT NULL AND expires_at<=?) OR created_at<=?)").bind(now.to_rfc3339()).bind(content).bind(audit).bind(now.to_rfc3339()).bind(pending_cutoff.to_rfc3339()).execute(pool).await?;
        }
        Database::Mysql(pool) => {
            sqlx::query("UPDATE messages SET status='expired',finished_at=?,content_retained_until=COALESCE(content_retained_until,?),audit_retained_until=COALESCE(audit_retained_until,?),last_error_code='message_expired' WHERE status IN ('queued','retry_wait','held') AND ((expires_at IS NOT NULL AND expires_at<=?) OR created_at<=?)").bind(now.naive_utc()).bind(mysql_content).bind(mysql_audit).bind(now.naive_utc()).bind(pending_cutoff.naive_utc()).execute(pool).await?;
        }
    }
    Ok(())
}

async fn claim_batch(db: &Database) -> Result<Vec<String>, sqlx::Error> {
    let now = Utc::now();
    let ids = match db {
        Database::Sqlite(pool) => sqlx::query_scalar::<_, String>("SELECT id FROM messages WHERE ((status IN ('queued','retry_wait') AND (next_attempt_at IS NULL OR next_attempt_at<=?)) OR (status='held' AND EXISTS (SELECT 1 FROM plugin_message_states s WHERE s.message_id=messages.id AND s.resume_at IS NOT NULL AND s.resume_at<=?))) AND (expires_at IS NULL OR expires_at>?) ORDER BY COALESCE(next_attempt_at,created_at),created_at LIMIT ?").bind(now.to_rfc3339()).bind(now.to_rfc3339()).bind(now.to_rfc3339()).bind(CLAIM_BATCH).fetch_all(pool).await?,
        Database::Mysql(pool) => sqlx::query_scalar::<_, String>("SELECT id FROM messages WHERE ((status IN ('queued','retry_wait') AND (next_attempt_at IS NULL OR next_attempt_at<=?)) OR (status='held' AND EXISTS (SELECT 1 FROM plugin_message_states s WHERE s.message_id=messages.id AND s.resume_at IS NOT NULL AND s.resume_at<=?))) AND (expires_at IS NULL OR expires_at>?) ORDER BY COALESCE(next_attempt_at,created_at),created_at LIMIT ?").bind(now.naive_utc()).bind(now.naive_utc()).bind(now.naive_utc()).bind(CLAIM_BATCH).fetch_all(pool).await?,
    };
    let mut claimed = Vec::with_capacity(ids.len());
    for id in ids {
        let affected = match db {
            Database::Sqlite(pool) => sqlx::query("UPDATE messages SET status='processing' WHERE id=? AND ((status IN ('queued','retry_wait') AND (next_attempt_at IS NULL OR next_attempt_at<=?)) OR (status='held' AND EXISTS (SELECT 1 FROM plugin_message_states s WHERE s.message_id=messages.id AND s.resume_at IS NOT NULL AND s.resume_at<=?))) AND (expires_at IS NULL OR expires_at>?)").bind(&id).bind(now.to_rfc3339()).bind(now.to_rfc3339()).bind(now.to_rfc3339()).execute(pool).await?.rows_affected(),
            Database::Mysql(pool) => sqlx::query("UPDATE messages SET status='processing' WHERE id=? AND ((status IN ('queued','retry_wait') AND (next_attempt_at IS NULL OR next_attempt_at<=?)) OR (status='held' AND EXISTS (SELECT 1 FROM plugin_message_states s WHERE s.message_id=messages.id AND s.resume_at IS NOT NULL AND s.resume_at<=?))) AND (expires_at IS NULL OR expires_at>?)").bind(&id).bind(now.naive_utc()).bind(now.naive_utc()).bind(now.naive_utc()).execute(pool).await?.rows_affected(),
        };
        if affected > 0 {
            claimed.push(id);
        }
    }
    Ok(claimed)
}

async fn mark_worker_failure(db: &Database, state: &AppState, id: &str) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    let (content, audit, mysql_content, mysql_audit) = retention_deadlines(state, now);
    match db {
        Database::Sqlite(pool) => {
            sqlx::query("UPDATE messages SET status='failed',finished_at=?,content_retained_until=COALESCE(content_retained_until,?),audit_retained_until=COALESCE(audit_retained_until,?),last_error_code='worker_failed' WHERE id=? AND status='processing'").bind(now.to_rfc3339()).bind(content).bind(audit).bind(id).execute(pool).await?;
        }
        Database::Mysql(pool) => {
            sqlx::query("UPDATE messages SET status='failed',finished_at=?,content_retained_until=COALESCE(content_retained_until,?),audit_retained_until=COALESCE(audit_retained_until,?),last_error_code='worker_failed' WHERE id=? AND status='processing'").bind(now.naive_utc()).bind(mysql_content).bind(mysql_audit).bind(id).execute(pool).await?;
        }
    }
    Ok(())
}

async fn clean_retained_data(db: &Database, state: &AppState) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    let admin_audit_cutoff = now - Duration::days(state.retention.audit_days);
    match db {
        Database::Sqlite(pool) => {
            let t = now.to_rfc3339();
            sqlx::query("UPDATE message_payloads SET retained_until=(SELECT content_retained_until FROM messages WHERE messages.id=message_payloads.message_id) WHERE retained_until IS NULL AND message_id IN (SELECT id FROM messages WHERE status IN ('smtp_accepted','rejected','failed','outcome_unknown','expired'))").execute(pool).await?;
            sqlx::query("DELETE FROM message_payloads WHERE retained_until IS NOT NULL AND retained_until<=?").bind(&t).execute(pool).await?;
            sqlx::query("DELETE FROM plugin_message_states WHERE message_id IN (SELECT id FROM messages WHERE status IN ('smtp_accepted','rejected','failed','outcome_unknown','expired') AND content_retained_until IS NOT NULL AND content_retained_until<=?)").bind(&t).execute(pool).await?;
            sqlx::query("DELETE FROM message_attempts WHERE message_id IN (SELECT id FROM messages WHERE audit_retained_until IS NOT NULL AND audit_retained_until<=?)").bind(&t).execute(pool).await?;
            sqlx::query("DELETE FROM message_events WHERE message_id IN (SELECT id FROM messages WHERE audit_retained_until IS NOT NULL AND audit_retained_until<=?)").bind(&t).execute(pool).await?;
            sqlx::query("DELETE FROM messages WHERE status IN ('smtp_accepted','rejected','failed','outcome_unknown','expired') AND audit_retained_until IS NOT NULL AND audit_retained_until<=?").bind(&t).execute(pool).await?;
            sqlx::query("DELETE FROM admin_audit_events WHERE created_at<=?")
                .bind(admin_audit_cutoff.to_rfc3339())
                .execute(pool)
                .await?;
        }
        Database::Mysql(pool) => {
            let t = now.naive_utc();
            sqlx::query("UPDATE message_payloads p JOIN messages m ON m.id=p.message_id SET p.retained_until=m.content_retained_until WHERE p.retained_until IS NULL AND m.status IN ('smtp_accepted','rejected','failed','outcome_unknown','expired')").execute(pool).await?;
            sqlx::query("DELETE FROM message_payloads WHERE retained_until IS NOT NULL AND retained_until<=?").bind(t).execute(pool).await?;
            sqlx::query("DELETE FROM plugin_message_states WHERE message_id IN (SELECT id FROM messages WHERE status IN ('smtp_accepted','rejected','failed','outcome_unknown','expired') AND content_retained_until IS NOT NULL AND content_retained_until<=?)").bind(t).execute(pool).await?;
            sqlx::query("DELETE FROM message_attempts WHERE message_id IN (SELECT id FROM messages WHERE audit_retained_until IS NOT NULL AND audit_retained_until<=?)").bind(t).execute(pool).await?;
            sqlx::query("DELETE FROM message_events WHERE message_id IN (SELECT id FROM messages WHERE audit_retained_until IS NOT NULL AND audit_retained_until<=?)").bind(t).execute(pool).await?;
            sqlx::query("DELETE FROM messages WHERE status IN ('smtp_accepted','rejected','failed','outcome_unknown','expired') AND audit_retained_until IS NOT NULL AND audit_retained_until<=?").bind(t).execute(pool).await?;
            sqlx::query("DELETE FROM admin_audit_events WHERE created_at<=?")
                .bind(admin_audit_cutoff.naive_utc())
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}
