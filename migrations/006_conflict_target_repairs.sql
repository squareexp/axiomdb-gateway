CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_app_key_env_unique
  ON projects(app_key, env);

CREATE UNIQUE INDEX IF NOT EXISTS idx_project_databases_database_name_unique
  ON project_databases(database_name);

CREATE UNIQUE INDEX IF NOT EXISTS idx_project_branches_project_branch_name_unique
  ON project_branches(project_id, branch_name);
