//! Authenticated encryption for persisted Hook signing credentials.

use std::{collections::BTreeMap, sync::Arc};

use aes_gcm::{
    AeadCore, Aes256Gcm, Key, KeyInit, Nonce,
    aead::{Aead, Payload, rand_core::OsRng},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::{ExposeSecret, SecretBox, SecretString};
use zeroize::Zeroizing;

/// AES-256-GCM ciphertext and the metadata needed to decrypt it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncryptedSecret {
    /// Version of the configured key used for encryption.
    pub key_version: i16,
    /// Unique 96-bit AES-GCM nonce.
    pub nonce: Vec<u8>,
    /// Ciphertext followed by the authentication tag.
    pub ciphertext: Vec<u8>,
}

/// Versioned AES-256-GCM credential cipher.
#[derive(Clone)]
pub struct SecretCipher {
    key_version: i16,
    key: Arc<SecretBox<[u8; 32]>>,
}

/// Current and retained historical ciphers used during key rotation.
#[derive(Clone, Debug)]
pub struct SecretCipherKeyring {
    current_version: i16,
    ciphers: BTreeMap<i16, SecretCipher>,
}

impl SecretCipherKeyring {
    /// Returns the version used for newly encrypted destination material.
    #[must_use]
    pub const fn current_version(&self) -> i16 {
        self.current_version
    }

    /// Decodes a validated base64url configuration keyring.
    ///
    /// # Errors
    ///
    /// Returns an error if the active version is absent or any key is not an
    /// exact 256-bit base64url value.
    pub fn from_base64url(
        current_version: i16,
        encoded_keys: &BTreeMap<i16, SecretString>,
    ) -> anyhow::Result<Self> {
        let mut ciphers = BTreeMap::new();
        for (version, encoded) in encoded_keys {
            let decoded = Zeroizing::new(
                URL_SAFE_NO_PAD
                    .decode(encoded.expose_secret())
                    .map_err(|_| {
                        anyhow::anyhow!("encryption key version {version} is malformed")
                    })?,
            );
            let key: [u8; 32] = decoded
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("encryption key version {version} is not 256 bits"))?;
            ciphers.insert(
                *version,
                SecretCipher::new(*version, SecretBox::new(Box::new(key))),
            );
        }
        if !ciphers.contains_key(&current_version) {
            anyhow::bail!("active encryption key version is missing");
        }
        Ok(Self {
            current_version,
            ciphers,
        })
    }

    /// Encrypts with the configured active key.
    ///
    /// # Errors
    ///
    /// Returns an error only if the keyring invariant is violated or encryption
    /// fails.
    pub fn encrypt(
        &self,
        secret: &SecretString,
        associated_data: &[u8],
    ) -> anyhow::Result<EncryptedSecret> {
        self.ciphers
            .get(&self.current_version)
            .ok_or_else(|| anyhow::anyhow!("active encryption cipher is unavailable"))?
            .encrypt(secret, associated_data)
    }

    /// Decrypts using the ciphertext's retained key version.
    ///
    /// # Errors
    ///
    /// Returns an error when the historical key is unavailable or
    /// authentication fails.
    pub fn decrypt(
        &self,
        encrypted: &EncryptedSecret,
        associated_data: &[u8],
    ) -> anyhow::Result<SecretString> {
        self.ciphers
            .get(&encrypted.key_version)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "encryption key version {} is unavailable",
                    encrypted.key_version
                )
            })?
            .decrypt(encrypted, associated_data)
    }
}

impl std::fmt::Debug for SecretCipher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretCipher")
            .field("key_version", &self.key_version)
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl SecretCipher {
    /// Creates a cipher using the active key version.
    #[must_use]
    pub fn new(key_version: i16, key: SecretBox<[u8; 32]>) -> Self {
        Self {
            key_version,
            key: Arc::new(key),
        }
    }

    /// Returns the active encryption-key version.
    #[must_use]
    pub const fn key_version(&self) -> i16 {
        self.key_version
    }

    /// Encrypts a secret and binds it to the supplied destination identity.
    ///
    /// # Errors
    ///
    /// Returns an error if authenticated encryption fails.
    pub fn encrypt(
        &self,
        secret: &SecretString,
        associated_data: &[u8],
    ) -> anyhow::Result<EncryptedSecret> {
        let cipher = self.cipher();
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: secret.expose_secret().as_bytes(),
                    aad: associated_data,
                },
            )
            .map_err(|_| anyhow::anyhow!("Hook credential encryption failed"))?;

        Ok(EncryptedSecret {
            key_version: self.key_version,
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    /// Decrypts and authenticates a stored secret.
    ///
    /// # Errors
    ///
    /// Returns an error for a wrong key version, malformed nonce, authentication
    /// failure, or non-UTF-8 plaintext.
    pub fn decrypt(
        &self,
        encrypted: &EncryptedSecret,
        associated_data: &[u8],
    ) -> anyhow::Result<SecretString> {
        if encrypted.key_version != self.key_version {
            anyhow::bail!(
                "unsupported Hook credential key version {}",
                encrypted.key_version
            );
        }
        let nonce_bytes: [u8; 12] = encrypted
            .nonce
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("stored Hook credential nonce is malformed"))?;
        let nonce = Nonce::from(nonce_bytes);
        let plaintext = self
            .cipher()
            .decrypt(
                &nonce,
                Payload {
                    msg: encrypted.ciphertext.as_slice(),
                    aad: associated_data,
                },
            )
            .map_err(|_| anyhow::anyhow!("Hook credential authentication failed"))?;
        let plaintext = Zeroizing::new(
            String::from_utf8(plaintext)
                .map_err(|_| anyhow::anyhow!("Hook credential plaintext is not UTF-8"))?,
        );
        Ok(SecretString::from(plaintext.as_str()))
    }

    fn cipher(&self) -> Aes256Gcm {
        let key = Key::<Aes256Gcm>::from_slice(self.key.expose_secret());
        Aes256Gcm::new(key)
    }
}

/// Builds unambiguous associated data for one organization/Silicon destination.
#[must_use]
pub fn destination_associated_data(org_id: &str, silicon_id: &str) -> Vec<u8> {
    let mut data = Vec::with_capacity(org_id.len() + silicon_id.len() + 1);
    data.extend_from_slice(org_id.as_bytes());
    data.push(0);
    data.extend_from_slice(silicon_id.as_bytes());
    data
}

/// Builds field-separated associated data for a destination credential column.
#[must_use]
pub fn destination_field_associated_data(
    org_id: &str,
    silicon_id: &str,
    field: &'static str,
) -> Vec<u8> {
    let mut data = destination_associated_data(org_id, silicon_id);
    data.push(0);
    data.extend_from_slice(field.as_bytes());
    data
}

#[cfg(test)]
mod tests {
    use secrecy::{ExposeSecret as _, SecretBox, SecretString};

    use super::{SecretCipher, destination_associated_data};

    #[test]
    fn round_trip_is_bound_to_destination() {
        let cipher = SecretCipher::new(1, SecretBox::new(Box::new([7_u8; 32])));
        let aad = destination_associated_data("org-a", "silicon-a");
        let encrypted = cipher.encrypt(&SecretString::from("hook-secret"), &aad);
        assert!(encrypted.is_ok());
        let Ok(encrypted) = encrypted else {
            return;
        };

        let decrypted = cipher.decrypt(&encrypted, &aad);
        assert!(decrypted.is_ok());
        let Ok(decrypted) = decrypted else {
            return;
        };
        assert_eq!(decrypted.expose_secret(), "hook-secret");

        let wrong_aad = destination_associated_data("org-b", "silicon-a");
        assert!(cipher.decrypt(&encrypted, &wrong_aad).is_err());
    }

    #[test]
    fn rejects_unknown_key_version() {
        let cipher = SecretCipher::new(2, SecretBox::new(Box::new([9_u8; 32])));
        let aad = destination_associated_data("org", "silicon");
        let encrypted = cipher.encrypt(&SecretString::from("secret"), &aad);
        assert!(encrypted.is_ok());
        let Ok(mut encrypted) = encrypted else {
            return;
        };
        encrypted.key_version = 1;
        assert!(cipher.decrypt(&encrypted, &aad).is_err());
    }
}
