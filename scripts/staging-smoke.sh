#!/bin/sh
set -eu
project="lumi-stage8-smoke-$$"
staging_port=${LUMI_STAGING_PORT:-}
if [ -z "$staging_port" ]; then
  staging_port=$(python3 -c 'import socket; sock = socket.socket(); sock.bind(("127.0.0.1", 0)); print(sock.getsockname()[1]); sock.close()')
fi
export LUMI_POSTGRES_PASSWORD='smoke:@/% password'
export LUMI_DATABASE_URL='postgres://lumi:smoke%3A%40%2F%25%20password@postgres:5432/lumi'
export LUMI_ADMIN_LOOKUP_IDS=''
export LUMI_STAGING_PORT="$staging_port"
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
curl --fail --silent "http://127.0.0.1:$staging_port/api/v1/health" >/dev/null
curl --fail --silent "http://127.0.0.1:$staging_port/api/v1/ready" >/dev/null
echo "staging image smoke passed"
