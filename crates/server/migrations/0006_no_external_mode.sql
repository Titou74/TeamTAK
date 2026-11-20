-- "External accounts only" is no longer a connection mode: external accesses can be added in
-- both remaining modes, so the mode added nothing.
--
-- A team in that mode moves to the shared identity. It keeps every access it has: it only held
-- external accesses, and those fit every mode. Its shared access is not created until the
-- leader asks for it, like any access.
UPDATE teams SET mode = 'shared' WHERE mode = 'external';

ALTER TABLE teams DROP CONSTRAINT teams_mode_check;
ALTER TABLE teams ADD CONSTRAINT teams_mode_check CHECK (mode IN ('shared', 'individual'));
