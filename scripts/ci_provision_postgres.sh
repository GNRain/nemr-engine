#!/usr/bin/env bash
#
# Provision Postgres for the WP-K sync acceptance, natively — no container.
#
# WHY NOT A SERVICE CONTAINER. The host job cannot use GitHub's `services:`
# block. Service containers are started by **Docker**, Docker runs on the
# system **containerd**, and `ci_provision_host.sh` disables containerd.service
# (PRIV-01: a socket-resolution mistake must not be able to reach a root
# daemon). So provisioning the Docker-free rootless stack pulls the rug from
# under the very container that was serving Postgres, and it dies mid-job —
# observed as "no Postgres listening on localhost:5432" after the provisioning
# step had already passed.
#
# That is not a flake to paper over: it is NFR-01 and GitHub's service model
# being genuinely incompatible in the same job. Installing from the Ubuntu
# archive sidesteps it and matches the posture the rest of provisioning already
# holds ("Ubuntu archive only — no Docker repository").
#
#   ./scripts/ci_provision_postgres.sh
#
# Leaves a Postgres on 127.0.0.1:5432 with role/db `nemr` (password `nemr`),
# matching the DATABASE_URL the workflow passes.

set -euo pipefail

echo "==> Install Postgres (Ubuntu archive; no container, no Docker)"
sudo apt-get update
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y postgresql

echo "==> Start the cluster"
sudo systemctl enable --now postgresql
# The unit returning is not the same as the server accepting connections.
for _ in $(seq 1 60); do
    sudo -u postgres pg_isready >/dev/null 2>&1 && break
    sleep 1
done
sudo -u postgres pg_isready || {
    sudo journalctl -u postgresql --no-pager -n 50 >&2 || true
    echo "postgres did not become ready" >&2
    exit 1
}

echo "==> Role and database"
# Idempotent: the script may run on a host that already has them.
sudo -u postgres psql -tAc "SELECT 1 FROM pg_roles WHERE rolname='nemr'" | grep -q 1 \
    || sudo -u postgres psql -c "CREATE ROLE nemr LOGIN PASSWORD 'nemr'"
sudo -u postgres psql -tAc "SELECT 1 FROM pg_database WHERE datname='nemr'" | grep -q 1 \
    || sudo -u postgres createdb -O nemr nemr

echo "==> Verify the connection the tests will actually use"
PGPASSWORD=nemr psql -h 127.0.0.1 -U nemr -d nemr -c 'SELECT 1' >/dev/null
echo "    ok — postgres://nemr@127.0.0.1:5432/nemr is reachable"
