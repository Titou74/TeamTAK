-- Anti-bot challenges already used to ask for a login code (crates/core/src/challenge.rs).
-- A challenge is signed, not stored, when it is handed out; it is written here when it comes
-- back with a request, so that it is accepted once. Rows go once the challenge has expired.
CREATE TABLE login_challenges (
    salt       text PRIMARY KEY,
    expires_at timestamptz NOT NULL
);
CREATE INDEX login_challenges_expires_idx ON login_challenges (expires_at);
