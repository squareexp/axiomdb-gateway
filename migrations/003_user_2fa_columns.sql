ALTER TABLE users
  ADD COLUMN IF NOT EXISTS two_factor_enabled BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN IF NOT EXISTS two_factor_method TEXT,
  ADD COLUMN IF NOT EXISTS two_factor_secret TEXT,
  ADD COLUMN IF NOT EXISTS two_factor_setup_completed BOOLEAN NOT NULL DEFAULT true;

UPDATE users
SET two_factor_setup_completed = true
WHERE two_factor_setup_completed IS NULL;
