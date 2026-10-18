#!/usr/bin/env bash
# A throwaway OpenLDAP directory for tests and demos (like the user-level sshd of demo-files.sh):
# runs as this user on 127.0.0.1, its own config, data, CA and certificate in DIR.
#
#   bash scripts/dev/slapd.sh start DIR LDAP_PORT LDAPS_PORT   # ready when it returns
#   bash scripts/dev/slapd.sh stop DIR                          # SIGTERM, then waits
#
# Needs slapd and openssl (apt-get install slapd; its system service can stay disabled). The
# binary is copied into DIR: Ubuntu's AppArmor profile for /usr/sbin/slapd would refuse a config
# and database outside /etc/ldap and /var/lib/ldap, and a copy runs unconfined.
#
# The directory (dc=example,dc=org; every password is "<uid>-password"):
#   cn=ductus-svc,ou=service              the service account (password svc-password)
#   uid=test-user-ada                     ductus-admins + ductus-phi + ductus-auditors
#   uid=test-user-olaf                    ductus-operators
#   uid=test-user-anna                    ductus-viewers
#   uid=test-user-nested                  lab-team, which is a member of ductus-operators
#   uid=test-user-nobody                  in no group
#   cn=admin,dc=example,dc=org            the directory's own admin (password admin-password),
#                                         for tests that change groups
set -euo pipefail
cmd=${1:?start|stop}
DIR=${2:?directory}
SLAPD=${SLAPD:-/usr/sbin/slapd}

if [ "$cmd" = stop ]; then
  if [ -f "$DIR/slapd.pid" ]; then
    pid=$(cat "$DIR/slapd.pid")
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 50); do kill -0 "$pid" 2>/dev/null || exit 0; sleep 0.1; done
    kill -KILL "$pid" 2>/dev/null || true
  fi
  exit 0
fi

PORT=${3:?ldap port}
TLS_PORT=${4:?ldaps port}
[ -x "$SLAPD" ] || { echo "slapd isn't installed (apt-get install slapd)" >&2; exit 2; }
mkdir -p "$DIR/data"
cp "$SLAPD" "$DIR/slapd"
ln -sf slapd "$DIR/slapadd"

# A CA and a certificate for localhost and 127.0.0.1, made for this run.
openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj "/CN=Ductus Test CA" \
  -keyout "$DIR/ca.key" -out "$DIR/ca.pem" 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout "$DIR/server.key" -out "$DIR/server.csr" 2>/dev/null
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\n' >"$DIR/san.ext"
openssl x509 -req -in "$DIR/server.csr" -CA "$DIR/ca.pem" -CAkey "$DIR/ca.key" -CAcreateserial -days 30 \
  -extfile "$DIR/san.ext" -out "$DIR/server.pem" 2>/dev/null

cat >"$DIR/slapd.conf" <<EOF
include /etc/ldap/schema/core.schema
include /etc/ldap/schema/cosine.schema
include /etc/ldap/schema/inetorgperson.schema
include /etc/ldap/schema/nis.schema
modulepath /usr/lib/ldap
moduleload back_mdb
pidfile $DIR/slapd.pid
TLSCACertificateFile $DIR/ca.pem
TLSCertificateFile $DIR/server.pem
TLSCertificateKeyFile $DIR/server.key
database mdb
maxsize 10485760
suffix "dc=example,dc=org"
rootdn "cn=admin,dc=example,dc=org"
rootpw admin-password
directory $DIR/data
access to attrs=userPassword by self write by anonymous auth by * none
access to * by * read
EOF

person() { # uid, name, mail
  printf 'dn: uid=%s,ou=people,dc=example,dc=org\nobjectClass: inetOrgPerson\nuid: %s\ncn: %s\nsn: %s\nmail: %s\nuserPassword: %s-password\n\n' \
    "$1" "$1" "$2" "${2##* }" "$3" "$1"
}
group() { # cn, members... (groupOfNames needs a member: a placeholder, so the last one can be removed)
  local cn=$1; shift
  printf 'dn: cn=%s,ou=groups,dc=example,dc=org\nobjectClass: groupOfNames\ncn: %s\nmember: cn=placeholder,dc=example,dc=org\n' "$cn" "$cn"
  for m in "$@"; do printf 'member: %s\n' "$m"; done
  printf '\n'
}
people=ou=people,dc=example,dc=org
groups=ou=groups,dc=example,dc=org
{
  printf 'dn: dc=example,dc=org\nobjectClass: dcObject\nobjectClass: organization\ndc: example\no: Example Hospital\n\n'
  for ou in people groups service; do printf 'dn: ou=%s,dc=example,dc=org\nobjectClass: organizationalUnit\nou: %s\n\n' "$ou" "$ou"; done
  printf 'dn: cn=ductus-svc,ou=service,dc=example,dc=org\nobjectClass: organizationalRole\nobjectClass: simpleSecurityObject\ncn: ductus-svc\nuserPassword: svc-password\n\n'
  person test-user-ada "Ada Lovelace" ada@example.org
  person test-user-olaf "Olaf Night" olaf@example.org
  person test-user-anna "Anna Smith" anna@example.org
  person test-user-nested "Nina Nested" nina@example.org
  person test-user-nobody "Noah Nobody" noah@example.org
  group ductus-admins "uid=test-user-ada,$people"
  group ductus-operators "uid=test-user-olaf,$people" "cn=lab-team,$groups"
  group ductus-viewers "uid=test-user-anna,$people"
  group ductus-phi "uid=test-user-ada,$people"
  group ductus-auditors "uid=test-user-ada,$people"
  group lab-team "uid=test-user-nested,$people"
} >"$DIR/data.ldif"
"$DIR/slapadd" -q -f "$DIR/slapd.conf" -l "$DIR/data.ldif"

"$DIR/slapd" -f "$DIR/slapd.conf" -h "ldap://127.0.0.1:$PORT/ ldaps://127.0.0.1:$TLS_PORT/" 2>"$DIR/slapd.log"
for _ in $(seq 1 100); do
  if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && (exec 3<>"/dev/tcp/127.0.0.1/$TLS_PORT") 2>/dev/null; then
    exit 0
  fi
  sleep 0.1
done
echo "slapd didn't start:" >&2
cat "$DIR/slapd.log" >&2
exit 1
