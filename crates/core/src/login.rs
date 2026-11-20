//! Rules of the passwordless login: one-time codes sent by e-mail.
//!
//! The functions here are pure; the server crate feeds them counts read from PostgreSQL.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Digits in a login code.
pub const CODE_LEN: usize = 6;

/// Limits applied to login codes. Every value comes from the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginPolicy {
    /// How long a code can be used, in seconds.
    pub code_ttl_secs: i64,
    /// Wrong attempts accepted on one code before it is discarded.
    pub max_attempts_per_code: i32,
    /// Sliding window of the two limits below, in seconds.
    pub window_secs: i64,
    /// Codes one address can request per window.
    pub max_codes_per_email: i64,
    /// Codes one IP address can request per window.
    pub max_codes_per_ip: i64,
    /// Wrong codes one IP address can submit per window, all addresses together.
    pub max_failures_per_ip: i64,
    /// Seconds to wait after a code was sent to an address before another one can be sent
    /// to it. The page shows the countdown on its "send a new code" button.
    pub resend_cooldown_secs: i64,
    /// Upper bound of the secret number of the anti-bot challenge (`challenge` module).
    pub challenge_max_number: u64,
}

impl Default for LoginPolicy {
    fn default() -> Self {
        Self {
            code_ttl_secs: 600,
            max_attempts_per_code: 5,
            window_secs: 3600,
            max_codes_per_email: 5,
            max_codes_per_ip: 30,
            max_failures_per_ip: 30,
            resend_cooldown_secs: 60,
            challenge_max_number: crate::challenge::DEFAULT_MAX_NUMBER,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDecision {
    Send,
    /// Too many codes requested for this address in the window.
    EmailLimited,
    /// Too many codes requested from this IP address in the window.
    IpLimited,
    /// A code was sent to this address less than `resend_cooldown_secs` ago: another one may
    /// be sent in `wait_secs` seconds.
    TooSoon { wait_secs: i64 },
}

impl LoginPolicy {
    /// Decides whether a new code may be sent, from the number of codes already requested in
    /// the current window for this address and from this IP address, and from the seconds
    /// since the last code was sent to this address (`None`: none in the window).
    pub fn check_request(&self, codes_for_email: i64, codes_from_ip: i64, secs_since_last: Option<f64>) -> RequestDecision {
        if codes_from_ip >= self.max_codes_per_ip {
            return RequestDecision::IpLimited;
        }
        if codes_for_email >= self.max_codes_per_email {
            return RequestDecision::EmailLimited;
        }
        match secs_since_last {
            // Rounded up: the page waits a whole second too many rather than one too few.
            Some(elapsed) if elapsed < self.resend_cooldown_secs as f64 => {
                RequestDecision::TooSoon { wait_secs: ((self.resend_cooldown_secs as f64 - elapsed).ceil() as i64).max(1) }
            }
            _ => RequestDecision::Send,
        }
    }

    /// True when this IP address has submitted too many wrong codes in the window.
    pub fn verification_blocked(&self, failures_from_ip: i64) -> bool {
        failures_from_ip >= self.max_failures_per_ip
    }

    /// True when a code has used up its attempts.
    pub fn code_exhausted(&self, attempts: i32) -> bool {
        attempts >= self.max_attempts_per_code
    }
}

/// A new numeric code, uniformly distributed, leading zeros kept.
pub fn generate_code() -> String {
    crate::random_string(CODE_LEN, b"0123456789")
}

/// Removes the spaces and dashes a user may type or paste around the digits.
pub fn normalize_code(input: &str) -> String {
    input.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// What is stored instead of the code: HMAC-SHA-256 of the address and the code under the
/// application key. A database leak alone does not reveal the six digits.
pub fn hash_code(key: &[u8], email: &str, code: &str) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(email.as_bytes());
    mac.update(b"\0");
    mac.update(code.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Constant-time comparison of two hashes of the same length.
pub fn hashes_equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_six_digits() {
        for _ in 0..200 {
            let c = generate_code();
            assert_eq!(c.len(), CODE_LEN);
            assert!(c.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn codes_are_not_constant() {
        let all: std::collections::HashSet<String> = (0..50).map(|_| generate_code()).collect();
        assert!(all.len() > 40);
    }

    #[test]
    fn pasted_codes_are_cleaned() {
        assert_eq!(normalize_code(" 123 456\n"), "123456");
        assert_eq!(normalize_code("12-34-56"), "123456");
        assert_eq!(normalize_code("abc"), "");
    }

    #[test]
    fn hash_depends_on_key_address_and_code() {
        let h = hash_code(b"k1", "a@b.fr", "123456");
        assert_eq!(h, hash_code(b"k1", "a@b.fr", "123456"));
        assert_ne!(h, hash_code(b"k2", "a@b.fr", "123456"));
        assert_ne!(h, hash_code(b"k1", "c@b.fr", "123456"));
        assert_ne!(h, hash_code(b"k1", "a@b.fr", "123457"));
        assert!(hashes_equal(&h, &h.clone()));
        assert!(!hashes_equal(&h, &hash_code(b"k1", "a@b.fr", "123457")));
    }

    #[test]
    fn request_limits() {
        let p = LoginPolicy { max_codes_per_email: 3, max_codes_per_ip: 10, ..Default::default() };
        assert_eq!(p.check_request(0, 0, None), RequestDecision::Send);
        assert_eq!(p.check_request(2, 9, Some(600.0)), RequestDecision::Send);
        assert_eq!(p.check_request(3, 0, Some(600.0)), RequestDecision::EmailLimited);
        assert_eq!(p.check_request(0, 10, None), RequestDecision::IpLimited);
        assert_eq!(p.check_request(3, 10, None), RequestDecision::IpLimited);
    }

    #[test]
    fn resend_cooldown() {
        let p = LoginPolicy { resend_cooldown_secs: 60, ..Default::default() };
        assert_eq!(p.check_request(1, 1, Some(0.2)), RequestDecision::TooSoon { wait_secs: 60 });
        assert_eq!(p.check_request(1, 1, Some(18.0)), RequestDecision::TooSoon { wait_secs: 42 });
        assert_eq!(p.check_request(1, 1, Some(59.9)), RequestDecision::TooSoon { wait_secs: 1 });
        assert_eq!(p.check_request(1, 1, Some(60.0)), RequestDecision::Send);
        // The hourly limit is the longer wait: it is the one reported.
        let p = LoginPolicy { max_codes_per_email: 1, ..p };
        assert_eq!(p.check_request(1, 1, Some(5.0)), RequestDecision::EmailLimited);
        // No cooldown at all.
        let p = LoginPolicy { resend_cooldown_secs: 0, ..Default::default() };
        assert_eq!(p.check_request(1, 1, Some(0.0)), RequestDecision::Send);
    }

    #[test]
    fn attempt_limits() {
        let p = LoginPolicy::default();
        assert!(!p.code_exhausted(4));
        assert!(p.code_exhausted(5));
        assert!(!p.verification_blocked(29));
        assert!(p.verification_blocked(30));
    }
}
