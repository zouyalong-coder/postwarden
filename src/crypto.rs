use std::{env, error::Error, fmt};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::{rngs::OsRng, RngCore};

const FORMAT_VERSION: u8 = 1;
const NONCE_LEN: usize = 12;

#[derive(Clone)]
pub struct Crypto {
    key: [u8; 32],
}

#[derive(Debug)]
pub struct CryptoError;

impl fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "encryption operation failed")
    }
}

impl Error for CryptoError {}

impl Crypto {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn from_env() -> Result<Self, Box<dyn Error>> {
        let encoded = env::var("POSTWARDEN_MASTER_KEY")
            .map_err(|_| "POSTWARDEN_MASTER_KEY must be set to a base64 encoded 32-byte key")?;
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| "POSTWARDEN_MASTER_KEY must be valid base64")?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "POSTWARDEN_MASTER_KEY must decode to exactly 32 bytes")?;
        Ok(Self::new(key))
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|_| CryptoError)?;
        let mut nonce = [0_u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .map_err(|_| CryptoError)?;
        let mut output = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        output.push(FORMAT_VERSION);
        output.extend_from_slice(&nonce);
        output.extend_from_slice(&ciphertext);
        Ok(output)
    }

    pub fn decrypt(&self, encrypted: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if encrypted.len() < 1 + NONCE_LEN + 16 || encrypted[0] != FORMAT_VERSION {
            return Err(CryptoError);
        }
        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|_| CryptoError)?;
        cipher
            .decrypt(
                Nonce::from_slice(&encrypted[1..1 + NONCE_LEN]),
                &encrypted[1 + NONCE_LEN..],
            )
            .map_err(|_| CryptoError)
    }
}

#[cfg(test)]
mod tests {
    use super::Crypto;

    #[test]
    fn ciphertext_changes_and_authenticates() {
        let crypto = Crypto { key: [7; 32] };
        let first = crypto.encrypt(b"smtp password").unwrap();
        let second = crypto.encrypt(b"smtp password").unwrap();
        assert_ne!(first, second);
        assert_eq!(crypto.decrypt(&first).unwrap(), b"smtp password");
        let mut tampered = first;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(crypto.decrypt(&tampered).is_err());
    }
}
