-- TeamTAK: an administrator may delete an account (the right to erasure of the terms of use).
--
-- What the account did for its teams outlives it, no longer tied to anyone: the teams it
-- asked for or decided on, and the invitations it sent that were answered. Its pending
-- invitations and its own team requests are removed with it (crates/server/src/admin.rs,
-- delete_account); here, the three references that still prevented the row from going.

ALTER TABLE teams ALTER COLUMN requested_by DROP NOT NULL;
ALTER TABLE teams DROP CONSTRAINT teams_requested_by_fkey;
ALTER TABLE teams ADD CONSTRAINT teams_requested_by_fkey FOREIGN KEY (requested_by) REFERENCES accounts (id) ON DELETE SET NULL;

ALTER TABLE teams DROP CONSTRAINT teams_decided_by_fkey;
ALTER TABLE teams ADD CONSTRAINT teams_decided_by_fkey FOREIGN KEY (decided_by) REFERENCES accounts (id) ON DELETE SET NULL;

ALTER TABLE invitations ALTER COLUMN invited_by DROP NOT NULL;
ALTER TABLE invitations DROP CONSTRAINT invitations_invited_by_fkey;
ALTER TABLE invitations ADD CONSTRAINT invitations_invited_by_fkey FOREIGN KEY (invited_by) REFERENCES accounts (id) ON DELETE SET NULL;
