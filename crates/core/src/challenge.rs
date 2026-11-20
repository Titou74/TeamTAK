//! Invisible anti-bot check asked before a login code is e-mailed: a small proof of work.
//!
//! The server draws a secret number between 0 and `max_number` and hands out the SHA-256 of
//! a random salt followed by that number, signed with the application key. The browser finds
//! the number by trying them all in the background while the user types the address (half a
//! million hashes on average with the default, a fraction of a second on a computer, about a
//! second on a phone), and sends it with the request. A script that only posts addresses
//! gets nothing: each code costs it a challenge fetched from the server and the work to
//! solve it, and a challenge is accepted once.
//!
//! The functions here are pure; the server crate records the challenges already used.

use hmac::{Hmac, KeyInit, Mac};
use rand::RngExt;
use sha2::{Digest, Sha256};

/// Name of the hash, as the browser's code names it.
pub const ALGORITHM: &str = "SHA-256";

/// How long a challenge can be used, in seconds. The page asks for a new one when it is
/// left open longer.
pub const TTL_SECS: i64 = 600;

/// Default upper bound of the secret number.
pub const DEFAULT_MAX_NUMBER: u64 = 1_000_000;

/// Bounds accepted for the configured `max_number`: below, the check costs a script nothing;
/// above, an old phone needs more than a few seconds.
pub const MAX_NUMBER_RANGE: std::ops::RangeInclusive<u64> = 10_000..=10_000_000;

/// What the browser is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// Random hex, a dot, and the expiry in Unix seconds: `3f9c…e2.1763660000`.
    pub salt: String,
    /// Lowercase hex SHA-256 of the salt followed by the secret number in decimal.
    pub challenge: String,
    /// The secret number is at most this.
    pub max_number: u64,
    /// HMAC of the salt and the challenge under the application key.
    pub signature: String,
}

/// Why a solution is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChallengeError {
    #[error("malformed challenge")]
    Malformed,
    #[error("challenge not issued by this server")]
    BadSignature,
    #[error("challenge expired")]
    Expired,
    #[error("wrong number")]
    WrongNumber,
}

fn signature(key: &[u8], salt: &str, challenge: &str) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    // Kept apart from the other uses of the application key.
    mac.update(b"teamtak login challenge\0");
    mac.update(salt.as_bytes());
    mac.update(b"\0");
    mac.update(challenge.as_bytes());
    mac
}

fn hash(salt: &str, number: u64) -> String {
    hex::encode(Sha256::new().chain_update(salt.as_bytes()).chain_update(number.to_string().as_bytes()).finalize())
}

/// A new challenge, valid `TTL_SECS` from `now` (Unix seconds).
pub fn issue(key: &[u8], now: i64, max_number: u64) -> Challenge {
    let salt = format!("{}.{}", crate::random_string(24, b"0123456789abcdef"), now + TTL_SECS);
    let secret = rand::rng().random_range(0..=max_number);
    let challenge = hash(&salt, secret);
    let signature = hex::encode(signature(key, &salt, &challenge).finalize().into_bytes());
    Challenge { salt, challenge, max_number, signature }
}

/// Checks a solution sent back by the browser, and returns the expiry of its challenge (Unix
/// seconds): the challenge must be refused again until then.
pub fn verify(key: &[u8], salt: &str, challenge: &str, number: u64, signature_hex: &str, now: i64) -> Result<i64, ChallengeError> {
    let expires = salt
        .rsplit_once('.')
        .filter(|(random, _)| random.len() == 24 && random.bytes().all(|b| b.is_ascii_hexdigit()))
        .and_then(|(_, expiry)| expiry.parse::<i64>().ok())
        .ok_or(ChallengeError::Malformed)?;
    let signature_bytes = hex::decode(signature_hex).map_err(|_| ChallengeError::Malformed)?;
    signature(key, salt, challenge).verify_slice(&signature_bytes).map_err(|_| ChallengeError::BadSignature)?;
    if expires <= now {
        return Err(ChallengeError::Expired);
    }
    if hash(salt, number) != challenge {
        return Err(ChallengeError::WrongNumber);
    }
    Ok(expires)
}

/// Finds the secret number, the way the browser does. For the tests and for scripts.
pub fn solve(salt: &str, challenge: &str, max_number: u64) -> Option<u64> {
    let target = hex::decode(challenge).ok()?;
    let prefix = Sha256::new().chain_update(salt.as_bytes());
    let mut digits = String::with_capacity(20);
    (0..=max_number).find(|n| {
        use std::fmt::Write;
        digits.clear();
        let _ = write!(digits, "{n}");
        prefix.clone().chain_update(digits.as_bytes()).finalize().as_slice() == target.as_slice()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
    const NOW: i64 = 1_763_660_000;

    fn solved(c: &Challenge) -> u64 {
        solve(&c.salt, &c.challenge, c.max_number).expect("a challenge always has a solution")
    }

    #[test]
    fn a_solved_challenge_is_accepted_until_it_expires() {
        let c = issue(KEY, NOW, 20_000);
        let n = solved(&c);
        assert!(n <= 20_000);
        assert_eq!(verify(KEY, &c.salt, &c.challenge, n, &c.signature, NOW), Ok(NOW + TTL_SECS));
        assert_eq!(verify(KEY, &c.salt, &c.challenge, n, &c.signature, NOW + TTL_SECS - 1), Ok(NOW + TTL_SECS));
        assert_eq!(verify(KEY, &c.salt, &c.challenge, n, &c.signature, NOW + TTL_SECS), Err(ChallengeError::Expired));
    }

    #[test]
    fn a_wrong_number_is_refused() {
        let c = issue(KEY, NOW, 20_000);
        let n = solved(&c);
        assert_eq!(verify(KEY, &c.salt, &c.challenge, n + 1, &c.signature, NOW), Err(ChallengeError::WrongNumber));
    }

    #[test]
    fn only_challenges_signed_by_this_server_count() {
        let c = issue(KEY, NOW, 20_000);
        let n = solved(&c);
        // Another installation's key.
        assert_eq!(verify(b"another key", &c.salt, &c.challenge, n, &c.signature, NOW), Err(ChallengeError::BadSignature));
        // A challenge made up by the client, with its own easy number.
        let forged = hash(&c.salt, 0);
        assert_eq!(verify(KEY, &c.salt, &forged, 0, &c.signature, NOW), Err(ChallengeError::BadSignature));
        // The expiry pushed back.
        let (random, _) = c.salt.rsplit_once('.').unwrap();
        let later = format!("{random}.{}", NOW + 100 * TTL_SECS);
        assert_eq!(verify(KEY, &later, &c.challenge, n, &c.signature, NOW), Err(ChallengeError::BadSignature));
    }

    #[test]
    fn garbage_is_malformed() {
        let c = issue(KEY, NOW, 20_000);
        for salt in ["", "abc", "nodot", "0123456789abcdef01234567.soon", "short.1763660600"] {
            assert_eq!(verify(KEY, salt, &c.challenge, 0, &c.signature, NOW), Err(ChallengeError::Malformed), "{salt:?}");
        }
        assert_eq!(verify(KEY, &c.salt, &c.challenge, 0, "not hex", NOW), Err(ChallengeError::Malformed));
    }

    #[test]
    fn challenges_differ() {
        let a = issue(KEY, NOW, 20_000);
        let b = issue(KEY, NOW, 20_000);
        assert_ne!(a.salt, b.salt);
        assert_eq!(a.salt.len(), 24 + 1 + 10);
        assert_eq!(a.challenge.len(), 64);
    }

    /// The browser's code (web/src/lib/challenge.ts) hashes the same bytes.
    #[test]
    fn hash_matches_the_reference() {
        assert_eq!(hash("salt.1", 42), hex::encode(Sha256::digest(b"salt.142")));
        assert_eq!(solve("salt.1", &hash("salt.1", 0), 10), Some(0));
        assert_eq!(solve("salt.1", &hash("salt.1", 10), 10), Some(10));
        assert_eq!(solve("salt.1", &hash("salt.1", 11), 10), None);
    }
}
