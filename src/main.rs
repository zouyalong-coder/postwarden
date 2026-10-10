mod auth;
mod config;
mod crypto;
mod database;
mod http;
mod smtp_accounts;

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
    if args.len() != 1 {
        return Err("usage: postwarden [init-admin <login-name>]".into());
    }

    let crypto = crypto::Crypto::from_env()?;
    let database = database::Database::connect(&config).await?;
    let app = http::router(
        &config.web_dist,
        http::AppState {
            database,
            crypto,
            cookie_secure: config.cookie_secure,
        },
    );
    let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;

    tracing::info!(
        address = %config.listen_addr,
        database = ?config.database_kind,
        "PostWarden HTTP service started"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
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
