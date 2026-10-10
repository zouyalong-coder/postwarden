use std::{env, net::SocketAddr, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseKind {
    Sqlite,
    Mysql,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub database_kind: DatabaseKind,
    pub database_url: String,
    pub web_dist: PathBuf,
    pub cookie_secure: bool,
    pub retention: RetentionPolicy,
}

#[derive(Debug, Clone, Copy)]
pub struct RetentionPolicy {
    pub pending_days: i64,
    pub content_days: i64,
    pub audit_days: i64,
}

impl RetentionPolicy {
    fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        fn days(name: &str) -> Result<i64, Box<dyn std::error::Error>> {
            let value = env::var(name)?.parse::<i64>()?;
            if !(0..=3650).contains(&value) {
                return Err(format!("{name} must be between 0 and 3650").into());
            }
            Ok(value)
        }
        fn optional_days(name: &str, default: i64) -> Result<i64, Box<dyn std::error::Error>> {
            match env::var(name) {
                Ok(_) => days(name),
                Err(env::VarError::NotPresent) => Ok(default),
                Err(e) => Err(e.into()),
            }
        }
        Ok(Self {
            pending_days: optional_days("POSTWARDEN_PENDING_DAYS", 7)?,
            content_days: optional_days("POSTWARDEN_CONTENT_DAYS", 7)?,
            audit_days: optional_days("POSTWARDEN_AUDIT_DAYS", 90)?,
        })
    }
}

impl Config {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let listen_addr = env::var("POSTWARDEN_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
            .parse()?;
        let database_url =
            env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/postwarden.db".to_owned());
        let database_kind = if database_url.starts_with("mysql://") {
            DatabaseKind::Mysql
        } else if database_url.starts_with("sqlite:") {
            DatabaseKind::Sqlite
        } else {
            return Err("DATABASE_URL must use sqlite:// or mysql://".into());
        };
        let web_dist = env::var_os("POSTWARDEN_WEB_DIST")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("frontend/dist"));
        let cookie_secure = match env::var("POSTWARDEN_COOKIE_SECURE") {
            Ok(value) if value == "false" => false,
            Ok(value) if value == "true" => true,
            Ok(_) => return Err("POSTWARDEN_COOKIE_SECURE must be true or false".into()),
            Err(env::VarError::NotPresent) => true,
            Err(error) => return Err(error.into()),
        };

        Ok(Self {
            listen_addr,
            database_kind,
            database_url,
            web_dist,
            cookie_secure,
            retention: RetentionPolicy::from_env()?,
        })
    }
}
