#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
control_db="${CONTROL_DB_URL:-${DATABASE_URL:-}}"
control_db_name="${CONTROL_DB_NAME:-}"
core_env="${ASTRADB_CORE_ENV:-/home/opsdc/.creds/astradb-core.env}"
gateway_role="${GATEWAY_DB_ROLE:-}"

if [[ -z "$control_db_name" && -f "$core_env" ]]; then
  database_url_line="$(grep -m1 '^DATABASE_URL=' "$core_env" || true)"
  if [[ -n "$database_url_line" ]]; then
    database_url="${database_url_line#DATABASE_URL=}"
    database_path="${database_url%%\?*}"
    control_db_name="${database_path##*/}"
    if [[ -z "$gateway_role" ]]; then
      gateway_role="$(printf '%s' "$database_url" | sed -E 's#^postgresql://([^:]+):.*#\1#')"
      if [[ "$gateway_role" == "$database_url" ]]; then
        gateway_role=""
      fi
    fi
  fi
fi

psql_vars=()
if [[ -n "$gateway_role" ]]; then
  psql_vars=(-v "gateway_role=${gateway_role}")
fi

run_psql() {
  if [[ -n "$control_db_name" ]] && command -v sudo >/dev/null 2>&1 && id postgres >/dev/null 2>&1; then
    sudo -u postgres psql "${psql_vars[@]}" -d "$control_db_name" "$@"
    return
  fi

  psql "${psql_vars[@]}" "${control_db:-postgresql://localhost/axiomdb_control}" "$@"
}

run_psql -v ON_ERROR_STOP=1 -f "$script_dir/migrations/001_schema_migrations.sql"
run_psql -v ON_ERROR_STOP=1 -c \
  "INSERT INTO schema_migrations(filename) VALUES ('001_schema_migrations.sql') ON CONFLICT (filename) DO NOTHING"

for f in "$script_dir"/migrations/*.sql; do
  fname="$(basename "$f")"
  exists="$(run_psql -v ON_ERROR_STOP=1 -tAc "SELECT 1 FROM schema_migrations WHERE filename='${fname}'")"
  if [[ -n "$exists" ]]; then
    echo "Skip ${fname} (already applied)"
    continue
  fi

  echo "Applying ${fname}"
  run_psql -v ON_ERROR_STOP=1 -f "$f"
  run_psql -v ON_ERROR_STOP=1 -c \
    "INSERT INTO schema_migrations(filename) VALUES ('${fname}')"
done

echo "Migrations complete."
