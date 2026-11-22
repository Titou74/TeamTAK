-- TeamTAK: acceptance of the terms of use.
--
-- Which version of the terms of use the account accepted last, and when. Signing in requires
-- accepting the version in force; an account whose version is older (or none, for accounts
-- made before the terms existed) is asked to accept them at its next visit.

ALTER TABLE accounts
    ADD COLUMN terms_version     text,
    ADD COLUMN terms_accepted_at timestamptz;
