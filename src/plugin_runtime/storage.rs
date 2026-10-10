//! Encrypted, namespaced key/value storage for plugins.
//!
//! Each plugin gets an isolated namespace identified by its registered plugin id.
//! Values are JSON and are encrypted before they reach the database.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    crypto::{Crypto, CryptoError},
    database::Database,
};
use postwarden_plugin_api::{async_trait, PluginError, PluginStore};

/// A plugin-scoped view over the shared application database.
#[derive(Clone)]
pub struct PluginStorage {
    database: Database,
    crypto: Crypto,
    plugin_id: String,
}

#[derive(Debug)]
pub enum PluginStorageError {
    Database(sqlx::Error),
    Serialization(serde_json::Error),
    Encryption(CryptoError),
    InvalidEnvelope,
    InvalidKey,
}

impl std::fmt::Display for PluginStorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "plugin storage database error: {error}"),
            Self::Serialization(error) => write!(formatter, "plugin storage JSON error: {error}"),
            Self::Encryption(error) => {
                write!(formatter, "plugin storage encryption error: {error}")
            }
            Self::InvalidEnvelope => write!(
                formatter,
                "plugin storage value failed namespace validation"
            ),
            Self::InvalidKey => write!(formatter, "plugin storage key must be 1 to 255 bytes"),
        }
    }
}

impl std::error::Error for PluginStorageError {}

impl From<sqlx::Error> for PluginStorageError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}
impl From<serde_json::Error> for PluginStorageError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}
impl From<CryptoError> for PluginStorageError {
    fn from(error: CryptoError) -> Self {
        Self::Encryption(error)
    }
}

#[derive(Serialize, Deserialize)]
struct StoredValue {
    plugin_id: String,
    storage_key: String,
    value: Value,
}

impl PluginStorage {
    /// Create a storage handle scoped to one registered plugin identifier.
    pub fn new(database: Database, crypto: Crypto, plugin_id: impl Into<String>) -> Self {
        Self {
            database,
            crypto,
            plugin_id: plugin_id.into(),
        }
    }

    /// Read and decrypt a JSON value. Returns `None` when the key is absent.
    pub async fn get(&self, key: &str) -> Result<Option<Value>, PluginStorageError> {
        Self::validate_key(key)?;
        let encrypted = match &self.database {
            Database::Sqlite(pool) => sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT encrypted_value FROM plugin_storage WHERE plugin_id = ? AND storage_key = ?",
            ).bind(&self.plugin_id).bind(key).fetch_optional(pool).await?,
            Database::Mysql(pool) => sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT encrypted_value FROM plugin_storage WHERE plugin_id = ? AND storage_key = ?",
            ).bind(&self.plugin_id).bind(key).fetch_optional(pool).await?,
        };
        let Some(encrypted) = encrypted else {
            return Ok(None);
        };
        let plaintext = self.crypto.decrypt(&encrypted)?;
        let stored: StoredValue = serde_json::from_slice(&plaintext)?;
        if stored.plugin_id != self.plugin_id || stored.storage_key != key {
            return Err(PluginStorageError::InvalidEnvelope);
        }
        Ok(Some(stored.value))
    }

    /// Encrypt and store a JSON value, replacing any previous value for this key.
    pub async fn put(&self, key: &str, value: &Value) -> Result<(), PluginStorageError> {
        Self::validate_key(key)?;
        let plaintext = serde_json::to_vec(&StoredValue {
            plugin_id: self.plugin_id.clone(),
            storage_key: key.to_owned(),
            value: value.clone(),
        })?;
        let encrypted = self.crypto.encrypt(&plaintext)?;
        match &self.database {
            Database::Sqlite(pool) => {
                sqlx::query("INSERT INTO plugin_storage (plugin_id, storage_key, encrypted_value) VALUES (?, ?, ?) ON CONFLICT(plugin_id, storage_key) DO UPDATE SET encrypted_value = excluded.encrypted_value")
                    .bind(&self.plugin_id).bind(key).bind(encrypted).execute(pool).await?;
            }
            Database::Mysql(pool) => {
                sqlx::query("INSERT INTO plugin_storage (plugin_id, storage_key, encrypted_value) VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE encrypted_value = VALUES(encrypted_value)")
                    .bind(&self.plugin_id).bind(key).bind(encrypted).execute(pool).await?;
            }
        }
        Ok(())
    }

    /// Delete a value. Returns whether a row was removed.
    pub async fn delete(&self, key: &str) -> Result<bool, PluginStorageError> {
        Self::validate_key(key)?;
        let rows_affected = match &self.database {
            Database::Sqlite(pool) => {
                sqlx::query("DELETE FROM plugin_storage WHERE plugin_id = ? AND storage_key = ?")
                    .bind(&self.plugin_id)
                    .bind(key)
                    .execute(pool)
                    .await?
                    .rows_affected()
            }
            Database::Mysql(pool) => {
                sqlx::query("DELETE FROM plugin_storage WHERE plugin_id = ? AND storage_key = ?")
                    .bind(&self.plugin_id)
                    .bind(key)
                    .execute(pool)
                    .await?
                    .rows_affected()
            }
        };
        Ok(rows_affected > 0)
    }

    fn validate_key(key: &str) -> Result<(), PluginStorageError> {
        if key.is_empty() || key.len() > 255 {
            Err(PluginStorageError::InvalidKey)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl PluginStore for PluginStorage {
    async fn get(&self, key: &str) -> Result<Option<Value>, PluginError> {
        PluginStorage::get(self, key)
            .await
            .map_err(|error| PluginError::new(error.to_string()))
    }

    async fn put(&self, key: &str, value: &Value) -> Result<(), PluginError> {
        PluginStorage::put(self, key, value)
            .await
            .map_err(|error| PluginError::new(error.to_string()))
    }

    async fn delete(&self, key: &str) -> Result<bool, PluginError> {
        PluginStorage::delete(self, key)
            .await
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::PluginStorage;
    use crate::{crypto::Crypto, database::Database};
    use postwarden_plugin_api::PluginStore;
    use serde_json::json;
    use sqlx::{sqlite::SqlitePoolOptions, Row};

    async fn test_database() -> Database {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations/sqlite")
            .run(&pool)
            .await
            .unwrap();
        Database::Sqlite(pool)
    }

    #[tokio::test]
    async fn encrypted_crud_is_scoped_to_plugin_id() {
        let database = test_database().await;
        let crypto = Crypto::new([42; 32]);
        let first = PluginStorage::new(database.clone(), crypto.clone(), "first-plugin");
        let second = PluginStorage::new(database.clone(), crypto, "second-plugin");
        let secret = json!({"token": "distinctive-secret-value"});

        assert_eq!(PluginStore::get(&first, "settings").await.unwrap(), None);
        PluginStore::put(&first, "settings", &secret).await.unwrap();
        assert_eq!(
            PluginStore::get(&first, "settings").await.unwrap(),
            Some(secret.clone())
        );
        assert_eq!(PluginStore::get(&second, "settings").await.unwrap(), None);

        let row = sqlx::query(
            "SELECT encrypted_value FROM plugin_storage WHERE plugin_id = ? AND storage_key = ?",
        )
        .bind("first-plugin")
        .bind("settings")
        .fetch_one(match &database {
            Database::Sqlite(pool) => pool,
            Database::Mysql(_) => unreachable!(),
        })
        .await
        .unwrap();
        let ciphertext: Vec<u8> = row.try_get("encrypted_value").unwrap();
        assert!(!ciphertext
            .windows(b"distinctive-secret-value".len())
            .any(|window| window == b"distinctive-secret-value"));

        let updated = json!([1, true, "replacement"]);
        PluginStore::put(&first, "settings", &updated)
            .await
            .unwrap();
        assert_eq!(
            PluginStore::get(&first, "settings").await.unwrap(),
            Some(updated)
        );
        assert!(PluginStore::delete(&first, "settings").await.unwrap());
        assert!(!PluginStore::delete(&first, "settings").await.unwrap());
        assert_eq!(PluginStore::get(&first, "settings").await.unwrap(), None);
    }
}
