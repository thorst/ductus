#!/usr/bin/env bash
# A throwaway Postgres with TLS for tests, demos and benchmarks (like slapd.sh): runs as this user
# on 127.0.0.1 and 127.0.0.2, its own data, CA and certificates in DIR.
#
#   bash scripts/dev/pgtls.sh start DIR PORT   # ready when it returns
#   bash scripts/dev/pgtls.sh reload DIR       # after replacing server.pem/.key: new connections get it
#   bash scripts/dev/pgtls.sh stop DIR         # fast shutdown, then waits
#
# Needs the Postgres server binaries (apt-get install postgresql; its system service can stay
# off): the newest /usr/lib/postgresql/*/bin, or PG_BIN. Exits 2 when there are none. Needs openssl.
#
# What's in DIR:
#   ca.pem                the CA that issued the server's certificate (for sslrootcert= or `ca:`)
#   other-ca.pem          a CA that issued nothing here (for "the wrong CA" tests)
#   server.pem/.key       the server's certificate: localhost and 127.0.0.1 (not 127.0.0.2: for
#                         "the wrong name" tests, the server listens there too)
#   client.pem/.key       a client certificate from ca.pem for the role `ductus_cert` (cert login)
#
# Logins: `ductus` (password `ductus`, superuser; plain or TLS) owns the database `ductus`;
# `ductus_cert` logs in only over TLS with client.pem (pg_hba `cert`). For example:
#   postgres://ductus:ductus@127.0.0.1:PORT/ductus?sslmode=verify-full&sslrootcert=DIR/ca.pem
#   postgres://ductus_cert@127.0.0.1:PORT/ductus?sslmode=verify-full&sslrootcert=DIR/ca.pem&sslcert=DIR/client.pem&sslkey=DIR/client.key
set -euo pipefail
cmd=${1:?start|stop}
DIR=${2:?directory}
BIN=${PG_BIN:-$(ls -d /usr/lib/postgresql/*/bin 2>/dev/null | sort -V | tail -n 1)}

if [ "$cmd" = stop ]; then
  if [ -f "$DIR/data/postmaster.pid" ]; then
    "$BIN/pg_ctl" -D "$DIR/data" -m fast -w -t 30 stop >/dev/null 2>&1 || true
  fi
  exit 0
fi
if [ "$cmd" = reload ]; then
  chmod 600 "$DIR"/*.key
  "$BIN/pg_ctl" -D "$DIR/data" reload >/dev/null
  exit 0
fi

PORT=${3:?port}
[ -n "$BIN" ] && [ -x "$BIN/initdb" ] || { echo "no Postgres server binaries (apt-get install postgresql, or set PG_BIN)" >&2; exit 2; }
mkdir -p "$DIR"

# A CA, the server's certificate from it, a client certificate from it, and a CA that issued nothing.
quiet() { "$@" 2>/dev/null >/dev/null; }
quiet openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=Ductus Test Postgres CA" -keyout "$DIR/ca.key" -out "$DIR/ca.pem"
quiet openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=Some Other CA" -keyout "$DIR/other-ca.key" -out "$DIR/other-ca.pem"
quiet openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout "$DIR/server.key" -out "$DIR/server.csr"
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\n' >"$DIR/server.ext"
quiet openssl x509 -req -in "$DIR/server.csr" -CA "$DIR/ca.pem" -CAkey "$DIR/ca.key" -CAcreateserial -days 365 \
  -extfile "$DIR/server.ext" -out "$DIR/server.pem"
quiet openssl req -newkey rsa:2048 -nodes -subj "/CN=ductus_cert" -keyout "$DIR/client.key" -out "$DIR/client.csr"
printf 'extendedKeyUsage=clientAuth\n' >"$DIR/client.ext"
quiet openssl x509 -req -in "$DIR/client.csr" -CA "$DIR/ca.pem" -CAkey "$DIR/ca.key" -CAcreateserial -days 365 \
  -extfile "$DIR/client.ext" -out "$DIR/client.pem"
chmod 600 "$DIR"/*.key # Postgres refuses a key others can read

printf 'ductus\n' >"$DIR/password"
"$BIN/initdb" -D "$DIR/data" -U ductus --pwfile="$DIR/password" --auth-local=trust --auth-host=scram-sha-256 \
  -E UTF8 --no-instructions >"$DIR/initdb.log" 2>&1 || { cat "$DIR/initdb.log" >&2; exit 1; }
cat >"$DIR/data/pg_hba.conf" <<EOF
local   all  all                       trust
hostssl all  ductus_cert  127.0.0.0/8  cert
host    all  ductus       127.0.0.0/8  scram-sha-256
EOF
cat >>"$DIR/data/postgresql.conf" <<EOF
listen_addresses = '127.0.0.1,127.0.0.2'
port = $PORT
unix_socket_directories = '$DIR'
ssl = on
ssl_cert_file = '$DIR/server.pem'
ssl_key_file = '$DIR/server.key'
ssl_ca_file = '$DIR/ca.pem'
EOF
"$BIN/pg_ctl" -D "$DIR/data" -l "$DIR/postgres.log" -w -t 30 start >/dev/null || { cat "$DIR/postgres.log" >&2; exit 1; }
"$BIN/psql" -q -h "$DIR" -p "$PORT" -U ductus -d postgres -v ON_ERROR_STOP=1 \
  -c "CREATE DATABASE ductus OWNER ductus" -c "CREATE ROLE ductus_cert LOGIN" >/dev/null
"$BIN/psql" -q -h "$DIR" -p "$PORT" -U ductus -d ductus -v ON_ERROR_STOP=1 -c "GRANT ALL ON SCHEMA public TO ductus_cert" >/dev/null
