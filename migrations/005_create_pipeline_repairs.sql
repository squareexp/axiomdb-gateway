CREATE UNIQUE INDEX IF NOT EXISTS idx_project_branches_project_branch_name
  ON project_branches(project_id, branch_name);

CREATE INDEX IF NOT EXISTS idx_jobs_project_action_status_created
  ON provisioning_jobs(project_id, action, status, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_projects_status_created
  ON projects(status, created_at DESC);

\if :{?gateway_role}
SELECT format('GRANT USAGE ON SCHEMA public TO %I', :'gateway_role') \gexec
SELECT format('GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO %I', :'gateway_role') \gexec
SELECT format('GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO %I', :'gateway_role') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO %I', :'gateway_role') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO %I', :'gateway_role') \gexec
\endif
