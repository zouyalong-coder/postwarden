mod admin_messages;
mod auth;
mod config;
mod crypto;
mod database;
mod http;
mod messages;
mod plugin_runtime;
mod plugins;
mod sender_identities;
mod smtp_accounts;
mod smtp_tests;
mod smtp_transport;
mod tokens;
mod worker;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let config = config::Config::from_env()?;
    if args.len() >= 2 && args[1] == "setup" {
        if args.len() != 3 {
            return Err("usage: postwarden setup <login-name>".into());
        }
        // Validate the master key during setup so a service with an unusable
        // encryption key is never presented as ready for local testing.
        let _ = crypto::Crypto::from_env()?;
        let database = database::Database::connect(&config).await?;
        println!("Database ready; migrations applied");
        if auth::has_admin(&database).await? {
            println!("Administrator already initialized; keeping the existing account");
            return Ok(());
        }
        let password = setup_password()?;
        auth::initialize_admin(&database, &args[2], &password).await?;
        println!("Administrator initialized");
        return Ok(());
    }
    if args.len() >= 2 && args[1] == "init-admin" {
        if args.len() != 3 {
            return Err("usage: postwarden init-admin <login-name>".into());
        }
        let password = rpassword::prompt_password("Administrator password: ")?;
        let confirmation = rpassword::prompt_password("Confirm password: ")?;
        if password != confirmation {
            return Err("passwords do not match".into());
        }
        let database = database::Database::connect(&config).await?;
        auth::initialize_admin(&database, &args[2], &password).await?;
        println!("Administrator initialized");
        return Ok(());
    }
    let reset_requested = args.len() == 2 && args[1] == "--reset-admin-password";
    if args.len() != 1 && !reset_requested {
        return Err("usage: postwarden [--reset-admin-password | setup <login-name> | init-admin <login-name>]".into());
    }

    let crypto = crypto::Crypto::from_env()?;
    let database = database::Database::connect(&config).await?;
    if reset_requested {
        let password = setup_password()?;
        auth::reset_admin_password(&database, &password).await?;
        println!("Administrator password reset; all administrator sessions revoked");
    }
    let plugins = plugin_runtime::PluginManager::new(database.clone(), crypto.clone()).await?;
    let state = http::AppState {
        database,
        crypto,
        cookie_secure: config.cookie_secure,
        plugins,
        retention: config.retention,
    };
    let worker = worker::spawn(state.clone());
    let app = http::router(&config.web_dist, state);
    let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;

    tracing::info!(
        address = %config.listen_addr,
        database = ?config.database_kind,
        "PostWarden HTTP service started"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    worker.abort();

    Ok(())
}

fn setup_password() -> Result<String, Box<dyn std::error::Error>> {
    if let Some(path) = std::env::var_os("POSTWARDEN_SETUP_PASSWORD_FILE") {
        let path = std::path::PathBuf::from(path);
        let metadata = std::fs::metadata(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(
                    "POSTWARDEN_SETUP_PASSWORD_FILE must be readable only by its owner".into(),
                );
            }
        }
        let password = std::fs::read_to_string(path)?;
        return Ok(password.trim_end_matches(['\r', '\n']).to_owned());
    }
    let password = rpassword::prompt_password("Administrator password (at least 12 characters): ")
        .map_err(|error| format!("cannot read terminal password ({error}); run in an interactive terminal or use POSTWARDEN_SETUP_PASSWORD_FILE"))?;
    let confirmation = rpassword::prompt_password("Confirm password: ")
        .map_err(|error| format!("cannot read terminal confirmation ({error}); run in an interactive terminal or use POSTWARDEN_SETUP_PASSWORD_FILE"))?;
    if password != confirmation {
        return Err("passwords do not match".into());
    }
    Ok(password)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install termination signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
