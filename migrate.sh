#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
control_db="${CONTROL_DB_URL:-${DATABASE_URL:-postgresql://localhost/axiomdb_control}}"

psql "$control_db" -v ON_ERROR_STOP=1 -f "$script_dir/migrations/001_schema_migrations.sql"
psql "$control_db" -v ON_ERROR_STOP=1 -c \
  "INSERT INTO schema_migrations(filename) VALUES ('001_schema_migrations.sql') ON CONFLICT (filename) DO NOTHING"

for f in "$script_dir"/migrations/*.sql; do
  fname="$(basename "$f")"
  exists="$(psql "$control_db" -v ON_ERROR_STOP=1 -tAc "SELECT 1 FROM schema_migrations WHERE filename='${fname}'")"
  if [[ -n "$exists" ]]; then
    echo "Skip ${fname} (already applied)"
    continue
  fi

  echo "Applying ${fname}"
  psql "$control_db" -v ON_ERROR_STOP=1 -f "$f"
  psql "$control_db" -v ON_ERROR_STOP=1 -c \
    "INSERT INTO schema_migrations(filename) VALUES ('${fname}')"
done

echo "Migrations complete."
