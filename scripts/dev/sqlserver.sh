#!/usr/bin/env bash
# A local SQL Server 2025 in Docker for the SQL Server tests and demos, presenting a certificate
# from a test CA (TLS verified, TDS 8 strict). 2025, not 2022: SQL Server 2022 on Linux (CU27)
# refuses TDS 8 strict whatever certificate and TLS versions it's given; 2025 accepts it (TLS 1.3).
# MSSQL_IMAGE picks another (mcr.microsoft.com/mssql/server:2022-latest: everything but strict).
#   bash scripts/dev/sqlserver.sh          # create or start it, wait until it accepts logins; prints
#                                          #   user:password@localhost:1433 (DUCTUS_TEST_SQLSERVER)
#   bash scripts/dev/sqlserver.sh start    # create or start it, don't wait (CI starts it early)
#   bash scripts/dev/sqlserver.sh dir      # prints the folder with its CA (DUCTUS_TEST_SQLSERVER_TLS)
#   bash scripts/dev/sqlserver.sh remove   # delete it
# check.sh calls this when the container exists, so the tests run whenever it's set up; CI runs
# it too (a service container can't be given files before it starts).
#
# What's in the folder (DUCTUS_TEST_SQLSERVER_TLS, default ~/.local/share/ductus-mssql):
#   ca.pem           the CA that issued the server's certificate (for `ca:`)
#   other-ca.pem     a CA that issued nothing here (for "the wrong CA" tests)
#   server.pem/.key  the server's certificate: localhost, 127.0.0.1 and sql1.ductus.test (for
#                    `name_in_certificate:`), not 127.0.0.2 (for "the wrong name" tests)
#   mssql.conf       mssql-conf's settings: network.tlscert and network.tlskey
# The server's own generated certificate can't be verified by anyone (its name is always
# SSL_Self_Signed_Fallback) and SQL Server refuses TDS 8 strict with it, hence the test CA. And SQL
# Server accepts strict only with a certificate for its own host name: the container is
# sql1.ductus.test.
#
# WSL stops the distro (and Docker with it) when no WSL command has run for a while, so the
# container restarts with Docker and this script waits for it to be ready again.
set -euo pipefail
NAME=ductus-mssql
PASSWORD=${MSSQL_SA_PASSWORD:-Ductus-dev-1}
IMAGE=${MSSQL_IMAGE:-mcr.microsoft.com/mssql/server:2025-latest}
DIR=${DUCTUS_TEST_SQLSERVER_TLS:-$HOME/.local/share/ductus-mssql}
# Containers made otherwise (before the certificate was mounted, another image) don't have this
# label: they're made again.
LABEL="ductus.setup=tls-$(printf '%s' "$IMAGE" | sed 's/[^a-z0-9.-]/-/g')"
DOCKER="docker"
docker info >/dev/null 2>&1 || DOCKER="sudo -n docker"
case "${1:-}" in
  remove) $DOCKER rm -f "$NAME" >/dev/null && echo "removed"; exit 0 ;;
  dir) echo "$DIR"; exit 0 ;;
esac

# The certificates, made again when they end within 40 days (doctor warns under 30).
mkdir -p "$DIR"
chmod 755 "$DIR"
renewed=0
if ! openssl x509 -in "$DIR/server.pem" -noout -checkend $((40 * 86400)) >/dev/null 2>&1; then
  quiet() { "$@" 2>/dev/null >/dev/null; }
  quiet openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=Ductus Test SQL Server CA" \
    -keyout "$DIR/ca.key" -out "$DIR/ca.pem"
  quiet openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=Some Other CA" \
    -keyout "$DIR/other-ca.key" -out "$DIR/other-ca.pem"
  # As Microsoft asks of a SQL Server certificate: CN = the server's host name, server
  # authentication, key encipherment (clients check the names in subjectAltName).
  quiet openssl req -newkey rsa:2048 -nodes -subj "/CN=sql1.ductus.test" -keyout "$DIR/server.key" -out "$DIR/server.csr"
  printf '%s\n' 'subjectAltName=DNS:localhost,DNS:sql1.ductus.test,IP:127.0.0.1' 'extendedKeyUsage=serverAuth' \
    'keyUsage=critical,digitalSignature,keyEncipherment' >"$DIR/server.ext"
  quiet openssl x509 -req -in "$DIR/server.csr" -CA "$DIR/ca.pem" -CAkey "$DIR/ca.key" -CAcreateserial -days 365 \
    -extfile "$DIR/server.ext" -out "$DIR/server.pem"
  # SQL Server runs as uid 10001 in the container: it must read the key (a test key only).
  chmod 644 "$DIR/server.key" "$DIR/server.pem"
  renewed=1
fi
cat >"$DIR/mssql.conf" <<EOF
[network]
tlscert = /etc/ductus-tls/server.pem
tlskey = /etc/ductus-tls/server.key
EOF

if [ -n "$($DOCKER ps -aq --filter "name=^$NAME\$")" ] && \
  [ -z "$($DOCKER ps -aq --filter "name=^$NAME\$" --filter "label=$LABEL")" ]; then
  $DOCKER rm -f "$NAME" >/dev/null
fi
if ! $DOCKER inspect "$NAME" >/dev/null 2>&1; then
  # SQL Server takes 80% of RAM by default; 3 GB is plenty for tests.
  $DOCKER run -d --name "$NAME" --hostname sql1.ductus.test --label "$LABEL" --restart unless-stopped --memory=4g \
    -e MSSQL_MEMORY_LIMIT_MB=3072 -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD="$PASSWORD" -p 1433:1433 \
    -v "$DIR:/etc/ductus-tls:ro" -v "$DIR/mssql.conf:/var/opt/mssql/mssql.conf" "$IMAGE" >/dev/null
elif [ "$renewed" = 1 ]; then
  $DOCKER restart "$NAME" >/dev/null # SQL Server reads its certificate at start
fi
$DOCKER start "$NAME" >/dev/null
[ "${1:-}" = start ] && exit 0
for _ in $(seq 1 120); do
  if $DOCKER exec "$NAME" /opt/mssql-tools18/bin/sqlcmd -C -S localhost -U sa -P "$PASSWORD" -Q "SELECT 1" -b \
    -o /dev/null >/dev/null 2>&1; then
    echo "sa:$PASSWORD@localhost:1433"
    exit 0
  fi
  sleep 1
done
echo "SQL Server did not start; see: $DOCKER logs $NAME" >&2
exit 1
