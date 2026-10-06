#!/usr/bin/env bash
# A local SQL Server 2022 in Docker for the SQL Server tests:
#   bash scripts/dev/sqlserver.sh          # create or start it, wait until it accepts logins
#   bash scripts/dev/sqlserver.sh remove   # delete it
# check.sh calls this when the container exists, so the tests run whenever it's set up.
#
# WSL stops the distro (and Docker with it) when no WSL command has run for a while, so the
# container restarts with Docker and this script waits for it to be ready again.
set -euo pipefail
NAME=ductus-mssql
PASSWORD=Ductus-dev-1
DOCKER="docker"
docker info >/dev/null 2>&1 || DOCKER="sudo -n docker"
if [ "${1:-}" = remove ]; then
  $DOCKER rm -f "$NAME" >/dev/null && echo "removed"
  exit 0
fi
if ! $DOCKER inspect "$NAME" >/dev/null 2>&1; then
  # SQL Server takes 80% of RAM by default; 3 GB is plenty for tests.
  $DOCKER run -d --name "$NAME" --restart unless-stopped -e MSSQL_MEMORY_LIMIT_MB=3072 \
    -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD="$PASSWORD" -p 1433:1433 \
    mcr.microsoft.com/mssql/server:2022-latest >/dev/null
fi
$DOCKER start "$NAME" >/dev/null
for _ in $(seq 1 90); do
  if $DOCKER exec "$NAME" /opt/mssql-tools18/bin/sqlcmd -C -S localhost -U sa -P "$PASSWORD" -Q "SELECT 1" -b \
    -o /dev/null >/dev/null 2>&1; then
    echo "sa:$PASSWORD@localhost:1433"
    exit 0
  fi
  sleep 1
done
echo "SQL Server did not start; see: $DOCKER logs $NAME" >&2
exit 1
