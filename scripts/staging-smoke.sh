#!/bin/sh
set -eu
project="lumi-stage8-smoke-$$"
export LUMI_POSTGRES_PASSWORD='smoke:@/% password'
export LUMI_DATABASE_URL='postgres://lumi:smoke%3A%40%2F%25%20password@postgres:5432/lumi'
compose="docker compose -p $project --env-file deployments/staging.env.example -f deployments/compose.staging.yaml"
cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    $compose ps -a || true
    $compose logs --no-color migrate server || true
  fi
  $compose down -v --remove-orphans
  exit "$status"
}
trap cleanup EXIT INT TERM
$compose up -d --build --wait server
curl --fail --silent http://127.0.0.1:8080/api/v1/health >/dev/null
curl --fail --silent http://127.0.0.1:8080/api/v1/ready >/dev/null
echo "staging image smoke passed"
