ALTER TABLE project_branches
  ADD COLUMN IF NOT EXISTS parent_branch_id UUID REFERENCES project_branches(id) ON DELETE SET NULL,
  ADD COLUMN IF NOT EXISTS is_default BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN IF NOT EXISTS protected BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN IF NOT EXISTS lifespan TEXT NOT NULL DEFAULT 'forever',
  ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS ttl_seconds BIGINT,
  ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

DO $$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM pg_constraint
    WHERE conname = 'project_branches_status_check'
      AND conrelid = 'project_branches'::regclass
  ) THEN
    ALTER TABLE project_branches DROP CONSTRAINT project_branches_status_check;
  END IF;
END $$;

ALTER TABLE project_branches
  ADD CONSTRAINT project_branches_status_check
  CHECK (status IN ('active','expired','deleting','deleted','failed'));

CREATE UNIQUE INDEX IF NOT EXISTS idx_branches_one_default_per_project
  ON project_branches(project_id)
  WHERE is_default = true AND deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_branches_expires_at
  ON project_branches(expires_at)
  WHERE expires_at IS NOT NULL AND status = 'active';

INSERT INTO project_branches
  (project_id, branch_name, database_name, source_database, status, created_by,
   is_default, protected, lifespan)
SELECT p.id, 'main', d.database_name, d.database_name, 'active', p.created_by,
       true, true, 'forever'
FROM projects p
JOIN LATERAL (
  SELECT database_name
  FROM project_databases
  WHERE project_id = p.id
  ORDER BY created_at ASC
  LIMIT 1
) d ON true
WHERE NOT EXISTS (
  SELECT 1
  FROM project_branches b
  WHERE b.project_id = p.id AND b.branch_name = 'main'
);

CREATE TABLE IF NOT EXISTS organizations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  name TEXT NOT NULL,
  slug TEXT UNIQUE NOT NULL,
  created_by UUID REFERENCES users(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS organization_members (
  organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL CHECK (role IN ('owner','admin','developer','viewer','billing')),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (organization_id, user_id)
);

CREATE TABLE IF NOT EXISTS organization_invitations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
  email TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('owner','admin','developer','viewer','billing')),
  token_hash TEXT NOT NULL,
  expires_at TIMESTAMPTZ NOT NULL,
  accepted_at TIMESTAMPTZ,
  revoked_at TIMESTAMPTZ,
  created_by UUID REFERENCES users(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS network_policies (
  project_id UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  mode TEXT NOT NULL DEFAULT 'restricted' CHECK (mode IN ('restricted','public_runtime','public_all')),
  revision BIGINT NOT NULL DEFAULT 1,
  last_applied_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS network_rules (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  branch_id UUID REFERENCES project_branches(id) ON DELETE CASCADE,
  cidr TEXT NOT NULL,
  label TEXT NOT NULL,
  ports TEXT NOT NULL CHECK (ports IN ('runtime','direct','both')),
  scope TEXT NOT NULL CHECK (scope IN ('project','branch')),
  expires_at TIMESTAMPTZ,
  created_by UUID REFERENCES users(id),
  source_ip TEXT,
  source_user_agent TEXT,
  deleted_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_network_rules_project_active
  ON network_rules(project_id, deleted_at, expires_at);

CREATE TABLE IF NOT EXISTS network_apply_events (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  project_id UUID REFERENCES projects(id) ON DELETE CASCADE,
  rule_id UUID REFERENCES network_rules(id) ON DELETE SET NULL,
  revision BIGINT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pending','applied','failed')),
  output JSONB NOT NULL DEFAULT '{}'::jsonb,
  error_text TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS credential_events (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  project_id UUID REFERENCES projects(id) ON DELETE CASCADE,
  branch_id UUID REFERENCES project_branches(id) ON DELETE CASCADE,
  actor_user_id UUID REFERENCES users(id),
  action TEXT NOT NULL,
  source_ip TEXT,
  source_user_agent TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

DO $$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM pg_constraint
    WHERE conname = 'provisioning_jobs_action_check'
      AND conrelid = 'provisioning_jobs'::regclass
  ) THEN
    ALTER TABLE provisioning_jobs DROP CONSTRAINT provisioning_jobs_action_check;
  END IF;
END $$;

ALTER TABLE provisioning_jobs
  ADD CONSTRAINT provisioning_jobs_action_check
  CHECK (action IN ('provision','deprovision','branch_create','branch_delete','smoke','restore_plan','restore'));
