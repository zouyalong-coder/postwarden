use std::{path::Path, str::FromStr};

use sqlx::{
    mysql::MySqlPoolOptions,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    MySqlPool, SqlitePool,
};

use crate::config::{Config, DatabaseKind};

#[derive(Clone)]
pub enum Database {
    Sqlite(SqlitePool),
    Mysql(MySqlPool),
}

impl Database {
    pub async fn connect(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        match config.database_kind {
            DatabaseKind::Sqlite => {
                ensure_sqlite_parent(&config.database_url)?;
                let options = SqliteConnectOptions::from_str(&config.database_url)?
                    .create_if_missing(true)
                    .foreign_keys(true);
                let pool = SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(options)
                    .await?;
                sqlx::migrate!("./migrations/sqlite").run(&pool).await?;
                Ok(Self::Sqlite(pool))
            }
            DatabaseKind::Mysql => {
                let pool = MySqlPoolOptions::new()
                    .max_connections(5)
                    .connect(&config.database_url)
                    .await?;
                sqlx::migrate!("./migrations/mysql").run(&pool).await?;
                Ok(Self::Mysql(pool))
            }
        }
    }

    pub async fn ping(&self) -> Result<(), sqlx::Error> {
        match self {
            Self::Sqlite(pool) => {
                sqlx::query("SELECT 1").execute(pool).await?;
            }
            Self::Mysql(pool) => {
                sqlx::query("SELECT 1").execute(pool).await?;
            }
        }
        Ok(())
    }
}

fn ensure_sqlite_parent(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
    else {
        return Ok(());
    };
    let path = path.split('?').next().unwrap_or(path);
    if path == ":memory:" || path.is_empty() {
        return Ok(());
    }
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}
