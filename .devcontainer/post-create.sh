#!/usr/bin/env bash
set -euo pipefail

repo_root="/workspaces/lumi"

sudo chown -R vscode:vscode /home/vscode/.codex
git config --global --add safe.directory "${repo_root}"
sudo npm install --global @openai/codex@latest

for attempt in $(seq 1 30); do
  if docker info >/dev/null 2>&1; then
    break
  fi
  if [ "${attempt}" -eq 30 ]; then
    echo "Nested Docker did not become ready" >&2
    exit 1
  fi
  sleep 1
done

npm --prefix tests/e2e ci
npm --prefix spikes/ai-web ci
cargo fetch
pre-commit install

codex --version
rustc --version
dx --version
docker compose version

if ! codex login status >/dev/null 2>&1; then
  echo
  echo "Codex is not authenticated in this devcontainer."
  echo "Run: codex login --device-auth"
fi
