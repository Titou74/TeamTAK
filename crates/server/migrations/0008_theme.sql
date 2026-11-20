-- Colour theme of the web UI, chosen on the profile page. 'auto' follows the device's light or
-- dark setting; 'light' and 'dark' force one.
ALTER TABLE accounts ADD COLUMN theme text NOT NULL DEFAULT 'auto' CHECK (theme IN ('auto', 'light', 'dark'));
