//! Password hashing: argon2id with the parameters from the configuration.

use std::sync::Arc;

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher as _, PasswordVerifier as _};
use argon2::{Algorithm, Argon2, Params, Version};
use thiserror::Error;

use crate::config::PasswordParams;

#[derive(Debug, Error)]
pub enum PasswordError {
    #[error("invalid argon2 parameters")]
    Params,
    #[error("hashing failed")]
    Hash,
    #[error("hashing task failed")]
    Task,
}

/// Hashes and verifies passwords on the blocking thread pool, so a slow hash never stalls the async runtime.
#[derive(Clone)]
pub struct PasswordHasher {
    argon2: Arc<Argon2<'static>>,
    /// Hash of a random throwaway secret at the same cost as real hashes. Login verifies against it for an
    /// unknown e-mail so the response time does not reveal whether the account exists.
    dummy_hash: Arc<str>,
}

impl PasswordHasher {
    pub fn new(params: PasswordParams) -> Result<Self, PasswordError> {
        let params = Params::new(params.m_cost_kib, params.t_cost, params.p_cost, None)
            .map_err(|_| PasswordError::Params)?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let throwaway = crate::security::new_opaque_token();
        let dummy = argon2
            .hash_password(throwaway.as_bytes())
            .map_err(|_| PasswordError::Hash)?;
        Ok(Self {
            argon2: Arc::new(argon2),
            dummy_hash: dummy.to_string().into(),
        })
    }

    pub fn dummy_hash(&self) -> &str {
        &self.dummy_hash
    }

    pub async fn hash(&self, password: &str) -> Result<String, PasswordError> {
        let (argon2, password) = (Arc::clone(&self.argon2), password.to_owned());
        tokio::task::spawn_blocking(move || {
            argon2
                .hash_password(password.as_bytes())
                .map(|hash| hash.to_string())
                .map_err(|_| PasswordError::Hash)
        })
        .await
        .map_err(|_| PasswordError::Task)?
    }

    /// Whether `password` matches `hash`; a malformed stored hash simply does not match.
    pub async fn verify(&self, password: &str, hash: &str) -> Result<bool, PasswordError> {
        let (argon2, password, hash) = (
            Arc::clone(&self.argon2),
            password.to_owned(),
            hash.to_owned(),
        );
        tokio::task::spawn_blocking(move || {
            PasswordHash::new(&hash)
                .is_ok_and(|parsed| argon2.verify_password(password.as_bytes(), &parsed).is_ok())
        })
        .await
        .map_err(|_| PasswordError::Task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hasher() -> PasswordHasher {
        PasswordHasher::new(PasswordParams {
            m_cost_kib: 8,
            t_cost: 1,
            p_cost: 1,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn hashes_with_a_random_salt_and_verifies() {
        let hasher = hasher();
        let (a, b) = (
            hasher.hash("correct horse").await.unwrap(),
            hasher.hash("correct horse").await.unwrap(),
        );
        assert_ne!(a, b);
        assert!(a.starts_with("$argon2id$"));
        assert!(hasher.verify("correct horse", &a).await.unwrap());
        assert!(!hasher.verify("wrong", &a).await.unwrap());
    }

    #[tokio::test]
    async fn a_malformed_hash_never_matches() {
        assert!(!hasher().verify("x", "not a hash").await.unwrap());
    }

    #[tokio::test]
    async fn the_dummy_hash_is_a_real_hash_nobody_knows() {
        let hasher = hasher();
        assert!(hasher.dummy_hash().starts_with("$argon2id$"));
        assert!(
            !hasher
                .verify("anything", hasher.dummy_hash())
                .await
                .unwrap()
        );
    }

    #[test]
    fn rejects_unusable_parameters() {
        let bad = PasswordParams {
            m_cost_kib: 0,
            t_cost: 0,
            p_cost: 0,
        };
        assert!(matches!(
            PasswordHasher::new(bad),
            Err(PasswordError::Params)
        ));
    }
}
