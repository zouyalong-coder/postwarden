use std::path::Path;

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::Serialize;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::{
    admin_messages, auth, config::RetentionPolicy, crypto::Crypto, database::Database, messages,
    plugin_runtime, sender_identities, smtp_accounts, smtp_tests, tokens,
};

#[derive(Clone)]
pub struct AppState {
    pub database: Database,
    pub crypto: Crypto,
    pub cookie_secure: bool,
    pub plugins: plugin_runtime::PluginManager,
    pub retention: RetentionPolicy,
}

pub fn router(web_dist: &Path, state: AppState) -> Router {
    let static_files =
        ServeDir::new(web_dist).not_found_service(ServeFile::new(web_dist.join("index.html")));

    let api = Router::new()
        .route("/health", get(health))
        .merge(messages::router())
        .nest(
            "/admin",
            Router::new()
                .merge(auth::router())
                .merge(smtp_accounts::router())
                .merge(smtp_tests::router())
                .merge(admin_messages::router())
                .merge(sender_identities::router())
                .merge(tokens::router())
                .merge(plugin_runtime::router()),
        )
        .fallback(api_not_found);

    let index = web_dist.join("index.html");
    Router::new()
        .route("/health", get(health))
        .nest("/api/v1", api)
        .route_service("/accounts", ServeFile::new(&index))
        .route_service("/tokens", ServeFile::new(&index))
        .route_service("/plugins", ServeFile::new(&index))
        .route_service("/plugins/{id}", ServeFile::new(&index))
        .route_service("/history", ServeFile::new(&index))
        .fallback_service(static_files)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    match state.database.ping().await {
        Ok(()) => (StatusCode::OK, Json(HealthResponse { status: "ok" })),
        Err(_) => {
            tracing::warn!("database health check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthResponse {
                    status: "unavailable",
                }),
            )
        }
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn api_not_found() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            code: "not_found",
            message: "API endpoint not found",
        }),
    )
}

#[derive(Serialize)]
struct ApiError {
    code: &'static str,
    message: &'static str,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use axum::{
        body::{to_bytes, Body},
        http::{header, Method, Request, Response, StatusCode},
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::{router, AppState};
    use crate::{
        auth,
        config::{Config, DatabaseKind, RetentionPolicy},
        crypto::Crypto,
        database::Database,
        plugin_runtime::PluginManager,
    };

    fn set_cookie(response: &Response<Body>, name: &str) -> String {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .find(|value| value.starts_with(&format!("{name}=")))
            .unwrap()
            .to_owned()
    }

    async fn request(
        app: &axum::Router,
        method: Method,
        path: &str,
        cookie: Option<&str>,
        csrf: Option<&str>,
        body: Option<Value>,
    ) -> Response<Body> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        if let Some(csrf) = csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
        let body = if let Some(body) = body {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        app.clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn admin_session_and_smtp_account_lifecycle() {
        let config = Config {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            database_kind: DatabaseKind::Sqlite,
            database_url: "sqlite::memory:".to_owned(),
            web_dist: PathBuf::from("frontend/dist"),
            cookie_secure: false,
            retention: RetentionPolicy {
                pending_days: 7,
                content_days: 7,
                audit_days: 90,
            },
        };
        let database = Database::connect(&config).await.unwrap();
        auth::initialize_admin(&database, "admin", "TestPassword123!")
            .await
            .unwrap();
        assert!(
            auth::initialize_admin(&database, "other", "TestPassword123!")
                .await
                .is_err()
        );
        let app = router(
            &config.web_dist,
            AppState {
                database: database.clone(),
                crypto: Crypto::new([9; 32]),
                cookie_secure: false,
                retention: config.retention,
                plugins: PluginManager::new(database.clone(), Crypto::new([9; 32]))
                    .await
                    .unwrap(),
            },
        );

        let list_path = "/api/v1/admin/smtp-accounts";
        assert_eq!(
            request(&app, Method::GET, list_path, None, None, None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let session = request(&app, Method::GET, "/api/v1/admin/session", None, None, None).await;
        let initial_csrf = set_cookie(&session, "postwarden_csrf");
        let login = json!({"login_name":"admin","password":"TestPassword123!"});
        assert_eq!(
            request(
                &app,
                Method::POST,
                "/api/v1/admin/session",
                Some(&initial_csrf),
                initial_csrf.split_once('=').map(|(_, value)| value),
                Some(json!({"login_name":"missing","password":"TestPassword123!"})),
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                "/api/v1/admin/session",
                Some(&initial_csrf),
                None,
                Some(login.clone()),
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        let response = request(
            &app,
            Method::POST,
            "/api/v1/admin/session",
            Some(&initial_csrf),
            initial_csrf.split_once('=').map(|(_, value)| value),
            Some(login),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let session_cookie = set_cookie(&response, "postwarden_session");
        let csrf_cookie = set_cookie(&response, "postwarden_csrf");
        let cookies = format!("{session_cookie}; {csrf_cookie}");
        let csrf_value = csrf_cookie.split_once('=').unwrap().1;

        let create = json!({
            "name":"Smoke SMTP", "host":"smtp.example.com", "port":587,
            "tls_mode":"starttls", "auth_type":"plain",
            "username":"mail@example.com", "credential":"test-secret"
        });
        let mut insecure = create.clone();
        insecure["tls_mode"] = json!("none");
        assert_eq!(
            request(
                &app,
                Method::POST,
                list_path,
                Some(&cookies),
                Some(csrf_value),
                Some(insecure),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                list_path,
                Some(&cookies),
                None,
                Some(create.clone())
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        let response = request(
            &app,
            Method::POST,
            list_path,
            Some(&cookies),
            Some(csrf_value),
            Some(create),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let payload: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert!(payload.get("credential").is_none());
        let id = payload["id"].as_str().unwrap();
        let Database::Sqlite(pool) = &database else {
            unreachable!()
        };
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO sender_identities(id,smtp_account_id,display_name,email_address,envelope_from,version,enabled,created_at,updated_at) VALUES('template_identity',?,'Template','template@example.com','template@example.com',1,1,?,?)")
            .bind(id).bind(&now).bind(&now).execute(pool).await.unwrap();
        let template_path = "/api/v1/admin/plugins/mail-template/templates";
        let template_input = json!({
            "scenario_id":"order.created", "name":"Order created", "enabled":true,
            "sender_identity_id":"template_identity", "subject_template":"Order {{order_id}}",
            "text_template":"Created {{order_id}}", "to":["receiver@example.com"]
        });
        assert_eq!(
            request(
                &app,
                Method::POST,
                template_path,
                Some(&cookies),
                None,
                Some(template_input.clone())
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        let created_template = request(
            &app,
            Method::POST,
            template_path,
            Some(&cookies),
            Some(csrf_value),
            Some(template_input),
        )
        .await;
        assert_eq!(created_template.status(), StatusCode::CREATED);
        let template: Value = serde_json::from_slice(
            &to_bytes(created_template.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        let template_id = template["id"].as_str().unwrap();
        assert_eq!(
            request(&app, Method::GET, template_path, Some(&cookies), None, None)
                .await
                .status(),
            StatusCode::OK
        );
        let plugin_path = "/api/v1/admin/plugins/mail-template";
        assert_eq!(
            request(
                &app,
                Method::PUT,
                "/api/v1/admin/plugins/example-policy",
                Some(&cookies),
                Some(csrf_value),
                Some(json!({"config":{"unknown_setting":true}}))
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &app,
                Method::PUT,
                plugin_path,
                Some(&cookies),
                Some(csrf_value),
                Some(json!({"enabled":false}))
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(&app, Method::GET, template_path, Some(&cookies), None, None)
                .await
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                template_path,
                Some(&cookies),
                Some(csrf_value),
                Some(json!({"scenario_id":"blocked"}))
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &app,
                Method::PUT,
                &format!("{template_path}/{template_id}"),
                Some(&cookies),
                Some(csrf_value),
                Some(json!({"name":"Blocked edit"}))
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &app,
                Method::DELETE,
                &format!("{template_path}/{template_id}"),
                Some(&cookies),
                Some(csrf_value),
                None
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &app,
                Method::PUT,
                plugin_path,
                Some(&cookies),
                Some(csrf_value),
                Some(json!({"enabled":true}))
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(
                &app,
                Method::DELETE,
                &format!("{template_path}/{template_id}"),
                Some(&cookies),
                Some(csrf_value),
                None
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
        let encrypted: Vec<u8> =
            sqlx::query_scalar("SELECT credential_ciphertext FROM smtp_accounts WHERE id = ?")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(!encrypted
            .windows(b"test-secret".len())
            .any(|w| w == b"test-secret"));

        let account_path = format!("{list_path}/{id}");
        let response = request(
            &app,
            Method::PUT,
            &account_path,
            Some(&cookies),
            Some(csrf_value),
            Some(json!({"name":"Renamed SMTP"})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let after_update: Vec<u8> =
            sqlx::query_scalar("SELECT credential_ciphertext FROM smtp_accounts WHERE id = ?")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(after_update, encrypted);
        let disable_path = format!("{account_path}/disable");
        assert_eq!(
            request(
                &app,
                Method::POST,
                &disable_path,
                Some(&cookies),
                Some(csrf_value),
                None,
            )
            .await
            .status(),
            StatusCode::OK
        );
        let enable_path = format!("{account_path}/enable");
        assert_eq!(
            request(
                &app,
                Method::POST,
                &enable_path,
                Some(&cookies),
                Some(csrf_value),
                None,
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(
                &app,
                Method::DELETE,
                &account_path,
                Some(&cookies),
                Some(csrf_value),
                None
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
        let encrypted: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT credential_ciphertext FROM smtp_accounts WHERE id = ?")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(encrypted.is_none());
        assert_eq!(
            request(&app, Method::GET, &account_path, Some(&cookies), None, None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(
                &app,
                Method::DELETE,
                "/api/v1/admin/session",
                Some(&cookies),
                Some(csrf_value),
                None
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            request(&app, Method::GET, list_path, Some(&cookies), None, None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn password_change_and_reset_revoke_sessions() {
        let config = Config {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            database_kind: DatabaseKind::Sqlite,
            database_url: "sqlite::memory:".to_owned(),
            web_dist: PathBuf::from("frontend/dist"),
            cookie_secure: false,
            retention: RetentionPolicy {
                pending_days: 7,
                content_days: 7,
                audit_days: 90,
            },
        };
        let database = Database::connect(&config).await.unwrap();
        auth::initialize_admin(&database, "admin", "OriginalPass123!")
            .await
            .unwrap();
        let app = router(
            &config.web_dist,
            AppState {
                database: database.clone(),
                crypto: Crypto::new([9; 32]),
                cookie_secure: false,
                retention: config.retention,
                plugins: PluginManager::new(database.clone(), Crypto::new([9; 32]))
                    .await
                    .unwrap(),
            },
        );
        let initial = request(&app, Method::GET, "/api/v1/admin/session", None, None, None).await;
        let initial_csrf = set_cookie(&initial, "postwarden_csrf");
        let login_path = "/api/v1/admin/session";
        let old_login = json!({"login_name":"admin","password":"OriginalPass123!"});
        let login = request(
            &app,
            Method::POST,
            login_path,
            Some(&initial_csrf),
            initial_csrf.split_once('=').map(|(_, value)| value),
            Some(old_login.clone()),
        )
        .await;
        assert_eq!(login.status(), StatusCode::OK);
        let session_cookie = set_cookie(&login, "postwarden_session");
        let csrf_cookie = set_cookie(&login, "postwarden_csrf");
        let cookies = format!("{session_cookie}; {csrf_cookie}");
        let csrf = csrf_cookie.split_once('=').unwrap().1;
        let path = "/api/v1/admin/password";
        let change =
            json!({"current_password":"OriginalPass123!","new_password":"ChangedPass123!"});
        assert_eq!(
            request(&app, Method::POST, path, None, None, Some(change.clone()))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                path,
                Some(&cookies),
                None,
                Some(change.clone())
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                path,
                Some(&cookies),
                Some(csrf),
                Some(json!({"current_password":"wrong","new_password":"ChangedPass123!"}))
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                path,
                Some(&cookies),
                Some(csrf),
                Some(json!({"current_password":"OriginalPass123!","new_password":"short"}))
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        let changed = request(
            &app,
            Method::POST,
            path,
            Some(&cookies),
            Some(csrf),
            Some(change),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::NO_CONTENT);
        assert!(set_cookie(&changed, "postwarden_session").contains("postwarden_session="));
        assert_eq!(
            request(
                &app,
                Method::GET,
                "/api/v1/admin/smtp-accounts",
                Some(&cookies),
                None,
                None
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                path,
                Some(&cookies),
                Some(csrf),
                Some(
                    json!({"current_password":"OriginalPass123!","new_password":"AnotherPass123!"})
                )
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                login_path,
                Some(&initial_csrf),
                initial_csrf.split_once('=').map(|(_, value)| value),
                Some(old_login)
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        let new_login = json!({"login_name":"admin","password":"ChangedPass123!"});
        let login = request(
            &app,
            Method::POST,
            login_path,
            Some(&initial_csrf),
            initial_csrf.split_once('=').map(|(_, value)| value),
            Some(new_login.clone()),
        )
        .await;
        assert_eq!(login.status(), StatusCode::OK);
        let new_cookie = set_cookie(&login, "postwarden_session");
        auth::reset_admin_password(&database, "ResetPassword123!")
            .await
            .unwrap();
        let new_csrf_cookie = set_cookie(&login, "postwarden_csrf");
        let new_cookies = format!("{new_cookie}; {new_csrf_cookie}");
        assert_eq!(
            request(
                &app,
                Method::POST,
                path,
                Some(&new_cookies),
                new_csrf_cookie.split_once('=').map(|(_, value)| value),
                Some(
                    json!({"current_password":"ChangedPass123!","new_password":"AnotherPass123!"})
                )
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                login_path,
                Some(&initial_csrf),
                initial_csrf.split_once('=').map(|(_, value)| value),
                Some(new_login)
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                login_path,
                Some(&initial_csrf),
                initial_csrf.split_once('=').map(|(_, value)| value),
                Some(json!({"login_name":"admin","password":"ResetPassword123!"}))
            )
            .await
            .status(),
            StatusCode::OK
        );
    }
}
