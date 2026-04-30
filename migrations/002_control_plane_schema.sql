CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE IF NOT EXISTS users (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  email TEXT UNIQUE NOT NULL,
  password_hash TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('owner','admin','operator','viewer')),
  is_active BOOLEAN NOT NULL DEFAULT true,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS projects (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  slug TEXT UNIQUE NOT NULL,
  name TEXT NOT NULL,
  app_key TEXT NOT NULL,
  env TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','archived','failed')),
  created_by UUID NOT NULL REFERENCES users(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(app_key, env)
);

CREATE TABLE IF NOT EXISTS project_databases (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  database_name TEXT NOT NULL UNIQUE,
  owner_role TEXT NOT NULL,
  runtime_role TEXT NOT NULL,
  readonly_role TEXT NOT NULL,
  runtime_key TEXT NOT NULL,
  direct_key TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS project_branches (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  branch_name TEXT NOT NULL,
  database_name TEXT NOT NULL UNIQUE,
  source_database TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','deleted','failed')),
  created_by UUID NOT NULL REFERENCES users(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(project_id, branch_name)
);

CREATE OR REPLACE FUNCTION enforce_branch_cap()
RETURNS trigger AS $$
DECLARE
  active_count INTEGER;
BEGIN
  SELECT count(*) INTO active_count
  FROM project_branches
  WHERE project_id = NEW.project_id AND status = 'active';

  IF active_count >= 10 THEN
    RAISE EXCEPTION 'branch limit exceeded: max 10 active branches per project';
  END IF;

  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_enforce_branch_cap ON project_branches;
CREATE TRIGGER trg_enforce_branch_cap
BEFORE INSERT ON project_branches
FOR EACH ROW EXECUTE FUNCTION enforce_branch_cap();

CREATE TABLE IF NOT EXISTS provisioning_jobs (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  project_id UUID REFERENCES projects(id) ON DELETE SET NULL,
  action TEXT NOT NULL CHECK (action IN ('provision','deprovision','branch_create','branch_delete','smoke')),
  status TEXT NOT NULL CHECK (status IN ('pending','running','succeeded','failed')),
  requested_by UUID NOT NULL REFERENCES users(id),
  request_payload JSONB NOT NULL DEFAULT '{}'::jsonb,
  output JSONB NOT NULL DEFAULT '{}'::jsonb,
  error_text TEXT,
  started_at TIMESTAMPTZ,
  finished_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS audit_events (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  actor_user_id UUID REFERENCES users(id),
  action TEXT NOT NULL,
  target_type TEXT NOT NULL,
  target_id TEXT,
  metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_projects_app_env ON projects(app_key, env);
CREATE INDEX IF NOT EXISTS idx_branches_project_status ON project_branches(project_id, status);
CREATE INDEX IF NOT EXISTS idx_jobs_status_created ON provisioning_jobs(status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_audit_created ON audit_events(created_at DESC);
