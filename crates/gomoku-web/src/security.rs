//! Opaque tokens (cookies, e-mail links): random, URL-safe, stored only as a SHA-256 hash.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// A token is 32 random bytes as base64url: 43 characters.
pub const TOKEN_LENGTH: usize = 43;

/// A fresh unguessable token for a cookie or a link.
pub fn new_opaque_token() -> String {
    let bytes: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The value stored in the database in place of the token: SHA-256 as 64 lowercase hex characters.
pub fn hash_opaque_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_url_safe() {
        let (a, b) = (new_opaque_token(), new_opaque_token());
        assert_ne!(a, b);
        assert_eq!(a.len(), TOKEN_LENGTH);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
    }

    #[test]
    fn hashing_is_stable_and_hex() {
        assert_eq!(
            hash_opaque_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hash_opaque_token("abc").len(), 64);
    }
}
