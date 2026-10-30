#!/usr/bin/env bash
# v0.5 demo: people sign in with the hospital's directory (here a throwaway OpenLDAP,
# scripts/dev/slapd.sh; real Active Directory is verified with the first pilot).
#   1. Directory people sign in through the same POST /session as local accounts: their account is
#      made at the first sign-in, role and permissions from their groups (nested groups count).
#   2. A wrong password, a name nobody has and an attempt to inject `*` all get the same answer;
#      someone in no Ductus group is told so.
#   3. The directory's certificate (ldaps:// with a private CA: `ca:` names a `certificates:` entry) is in the certificate table.
#   4. Removed from the group: the next refresh (1m here) ends the session, logged; put back,
#      they sign in again.
#   5. Renamed in the directory (a modrdn of the sign-in name): the refresh finds them by
#      entryUUID and renames the account; the new name signs in to the same account.
#   6. A departed person's name given to someone new: a new account; the earlier one is moved
#      aside (`<name>#<id>`) and refused, its session ended.
#   7. The directory goes down: directory people can't sign in (503, said plainly), sessions
#      already open keep working, and the local break-glass admin still signs in.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO=$PWD
source scripts/dev/db.sh

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
P=$(mktemp -d)
L=$(mktemp -d /tmp/ductus-ldap.XXXX)
export DUCTUS_DATABASE_URL=$DB
API=http://127.0.0.1:7798
LDAP_PORT=3390 LDAPS_PORT=6637
q() { psql -tA "$DB" -c "$1"; }
fail=""
PEOPLE="'test-user-ada','test-user-olaf','test-user-olafn','test-user-anna','test-user-nested','test-user-nobody','test-user-breakglass'"
# At the start (what a killed run left) and at the end.
forget() {
  forget_test_users test-user-ada test-user-olaf test-user-olafn test-user-anna test-user-nested test-user-nobody \
    test-user-breakglass
  psql -q "$DB" -c "DELETE FROM certificate_status WHERE resource = 'corp-directory'"
}
forget
# Audit rows outlive the accounts: this run's are the ones after this.
START=$(q "SELECT now()")

ENGINE=""
cleanup() {
  [ -n "$ENGINE" ] && { kill -INT "$ENGINE" 2>/dev/null || true; wait "$ENGINE" 2>/dev/null || true; }
  bash "$REPO/scripts/dev/slapd.sh" stop "$L" || true
  rm -rf "$L" || true
  forget || true
}
trap cleanup EXIT
bash scripts/dev/slapd.sh start "$L" $LDAP_PORT $LDAPS_PORT

mkdir -p "$P/interfaces/ldap-demo"
cat >"$P/ductus.yaml" <<EOF
api: { listen: 127.0.0.1:7798 }
doctor: { schedule: off }
auth:
  ldap:
    resource: corp-directory
    user_base: ou=people,dc=example,dc=org
    user_filter: (uid={username})
    attributes: { display_name: cn, email: mail }
    nested: true
    refresh: 1m
    groups:
      admin: cn=ductus-admins,ou=groups,dc=example,dc=org
      operator: cn=ductus-operators,ou=groups,dc=example,dc=org
      viewer: cn=ductus-viewers,ou=groups,dc=example,dc=org
      permissions:
        payloads: cn=ductus-phi,ou=groups,dc=example,dc=org
        audit: cn=ductus-auditors,ou=groups,dc=example,dc=org
EOF
cat >"$P/resources.yaml" <<EOF
ldap:
  corp-directory:
    url: ldaps://127.0.0.1:$LDAPS_PORT
    bind_dn: cn=ductus-svc,ou=service,dc=example,dc=org
    bind_password: svc-password
    owner: Directory Services
    application: Example Directory
    ca: directory-ca
certificates:
  directory-ca: { ca_file: $L/ca.pem }
EOF
cat >"$P/interfaces/ldap-demo/interface.yaml" <<'EOF'
name: ldap-demo
source: { mllp: { port: 6737 } }
script: transform.ts
destination: { file: { dir: out } }
EOF
cat >"$P/interfaces/ldap-demo/transform.ts" <<'EOF'
import type { Message } from "@ductus/sdk";
export function transform(msg: Message) {
  return msg;
}
EOF
cd "$P"

# The break-glass admin: a local account, for when the directory is down.
echo "break-glass-password-1" | "$BIN" user create test-user-breakglass --role admin --password-stdin >/dev/null

"$BIN" run >"$P/run.log" 2>&1 &
ENGINE=$!
until grep "started interfaces=" "$P/run.log" 2>/dev/null >/dev/null; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
echo "   engine log: $P/run.log"

# A browser's sign-in: the session cookie comes back in Set-Cookie. Prints the HTTP status;
# the cookie goes to $P/<name>.cookie and the answer to $P/answer.
signin() { # username password
  local code
  code=$(curl -s -D "$P/headers" -o "$P/answer" -w '%{http_code}' -H 'X-Ductus: 1' -H 'Content-Type: application/json' \
    -d "{\"username\": \"$1\", \"password\": \"$2\"}" "$API/api/v1/session")
  local cookie
  cookie=$(sed -n 's/^[Ss]et-[Cc]ookie: ductus_session=\([^;]*\).*/\1/p' "$P/headers" | tr -d '\r')
  # A refused sign-in sets no cookie: the session from before stays the one to use.
  [ -n "$cookie" ] && echo "$cookie" >"$P/$1.cookie"
  echo "$code"
}
as() { # username path → status and body in $P/answer
  curl -s -o "$P/answer" -w '%{http_code}' -H "Cookie: ductus_session=$(cat "$P/$1.cookie")" "$API/api/v1$2"
}
js() { python3 -c "import json, sys; d = json.load(open(sys.argv[1])); print($1)" "$P/answer"; }
wait_for() { # seconds condition
  local end=$((SECONDS + $1))
  until eval "$2"; do
    [ $SECONDS -ge $end ] && return 1
    sleep 1
  done
}

echo "== directory people sign in (accounts made at the first sign-in)"
for who in ada olaf anna nested; do
  code=$(signin "test-user-$who" "test-user-$who-password")
  [ "$code" = 200 ] || fail+=" sign-in-$who($code $(cat "$P/answer"))"
  echo "   test-user-$who: $code, $(js "f\"{d['role']} {','.join(d['permissions']) or '-'}\"")"
done
as test-user-ada /session >/dev/null
[ "$(js "d['role'], sorted(d['permissions'])")" = "admin ['audit', 'payloads']" ] || fail+=" ada($(cat "$P/answer"))"
as test-user-nested /session >/dev/null
[ "$(js "d['role']")" = operator ] || fail+=" nested-groups"
as test-user-anna /session >/dev/null
[ "$(js "d['role'], d['kind']")" = "viewer user" ] || fail+=" anna"
row=$(q "SELECT source || ' ' || display_name || ' ' || email FROM users WHERE username = 'test-user-anna'")
echo "   users row: $row"
[ "$row" = "ldap Anna Smith anna@example.org" ] || fail+=" users-row($row)"
as test-user-ada /users >/dev/null
[ "$(js "[u['source'] for u in d if u['username'] == 'test-user-olaf']")" = "['ldap']" ] || fail+=" users-api"

echo "== refused alike: wrong password, unknown name, \`*\`; no group is told"
for attempt in "test-user-anna:wrong" "nobody-by-that-name:x" "*:test-user-anna-password"; do
  code=$(signin "${attempt%%:*}" "${attempt#*:}")
  [ "$code:$(js "d['error']")" = "401:wrong username or password" ] || fail+=" refused-$attempt($code)"
done
code=$(signin test-user-nobody test-user-nobody-password)
echo "   test-user-nobody: $code, $(js "d['error']")"
[ "$code" = 403 ] || fail+=" no-group($code)"
grep -i "password" "$P/run.log" | grep -E "test-user-[a-z]+-password|svc-password" && fail+=" password-in-the-log"

echo "== the directory's certificate is watched like the others"
code=$(as test-user-ada /certificates)
echo "   $(js "[f\"{c['resource']} {c['host']}:{c['port']} {c['state']}\" for c in d['certificates']]")"
curl -s -o "$P/answer" -X POST -H 'X-Ductus: 1' -H 'Content-Type: application/json' \
  -H "Cookie: ductus_session=$(cat "$P/test-user-ada.cookie")" -d '{"mode": "certificates"}' "$API/api/v1/doctor"
[ "$(js "[c['certificate']['trusted'] for c in d['checks'] if c['what'] == 'directory corp-directory']")" = "[True]" ] ||
  fail+=" doctor($(head -c 400 "$P/answer"))"
as test-user-ada /certificates >/dev/null
cert=$(js "[f\"{c['resource']} {c['days_left']} {c['trusted']}\" for c in d['certificates'] if c['resource'] == 'corp-directory']")
echo "   GET /certificates: $cert"
[ "$cert" = "['corp-directory 29 True']" ] || [ "$cert" = "['corp-directory 30 True']" ] || fail+=" certificates($cert)"

echo "== removed from the group: the next refresh ends the session"
as test-user-anna /session >/dev/null
ldapmodify -x -H "ldap://127.0.0.1:$LDAP_PORT" -D cn=admin,dc=example,dc=org -w admin-password >/dev/null <<'EOF'
dn: cn=ductus-viewers,ou=groups,dc=example,dc=org
changetype: modify
delete: member
member: uid=test-user-anna,ou=people,dc=example,dc=org
EOF
started=$SECONDS
wait_for 90 '[ "$(as test-user-anna /session)" = 401 ]' || fail+=" still-signed-in"
echo "   anna's session ended $((SECONDS - started)) s after the change (refresh: 1m)"
logged=$(q "SELECT actor_name || ' ' || (detail->>'reason') FROM audit_log WHERE action = 'user.directory_refused' AND subject = 'test-user-anna' AND at >= '$START'")
echo "   audit: $logged"
[ "$logged" = "engine in no Ductus group" ] || fail+=" refused-row($logged)"
[ "$(as test-user-olaf /session)" = 200 ] || fail+=" olaf-signed-out"
ldapmodify -x -H "ldap://127.0.0.1:$LDAP_PORT" -D cn=admin,dc=example,dc=org -w admin-password >/dev/null <<'EOF'
dn: cn=ductus-viewers,ou=groups,dc=example,dc=org
changetype: modify
add: member
member: uid=test-user-anna,ou=people,dc=example,dc=org
EOF
code=$(signin test-user-anna test-user-anna-password)
echo "   put back, anna signs in again: $code"
[ "$code" = 200 ] || fail+=" back($code)"
[ -z "$(q "SELECT directory_refused_at FROM users WHERE username = 'test-user-anna'")" ] || fail+=" still-refused"

ldap_admin() { # ldapmodify as the directory's admin, the LDIF on stdin
  ldapmodify -x -H "ldap://127.0.0.1:$LDAP_PORT" -D cn=admin,dc=example,dc=org -w admin-password >/dev/null
}
olaf_id=$(q "SELECT id FROM users WHERE username = 'test-user-olaf'")
anna_id=$(q "SELECT id FROM users WHERE username = 'test-user-anna'")

echo "== renamed in the directory (test-user-olaf → test-user-olafn): one account"
ldap_admin <<'EOF'
dn: uid=test-user-olaf,ou=people,dc=example,dc=org
changetype: modrdn
newrdn: uid=test-user-olafn
deleteoldrdn: 1
EOF
code=$(signin test-user-olaf test-user-olaf-password)
[ "$code" = 401 ] || fail+=" old-name($code)"
[ "$(as test-user-olaf /session)" = 200 ] || fail+=" old-name-ended-the-session"
started=$SECONDS
wait_for 90 '[ "$(q "SELECT username FROM users WHERE id = $olaf_id")" = test-user-olafn ]' || fail+=" not-renamed"
echo "   the refresh renamed the account $((SECONDS - started)) s after the change (refresh: 1m)"
as test-user-olaf /session >/dev/null
echo "   his session from before: $(js "d['name']")"
[ "$(js "d['name']")" = test-user-olafn ] || fail+=" renamed-session($(cat "$P/answer"))"
code=$(signin test-user-olafn test-user-olaf-password)
accounts=$(q "SELECT count(*) FROM users WHERE username LIKE 'test-user-olaf%'")
echo "   test-user-olafn signs in: $code; accounts for him: $accounts"
[ "$code:$accounts" = "200:1" ] || fail+=" renamed-sign-in($code $accounts)"
renamed=$(q "SELECT detail->'username' FROM audit_log WHERE action = 'user.directory_change' AND subject = 'test-user-olafn' AND at >= '$START'")
echo "   audit: $renamed"
[ "$renamed" = '["test-user-olaf", "test-user-olafn"]' ] || fail+=" renamed-row($renamed)"
[ "$(q "SELECT value FROM user_names WHERE user_id = $olaf_id AND kind = 'username'")" = test-user-olaf ] || fail+=" former-name"

echo "== test-user-anna leaves, and a new Anna gets her name: a new account; the old one stays out"
cp "$P/test-user-anna.cookie" "$P/earlier-anna.cookie"
ldapdelete -x -H "ldap://127.0.0.1:$LDAP_PORT" -D cn=admin,dc=example,dc=org -w admin-password \
  uid=test-user-anna,ou=people,dc=example,dc=org
ldap_admin <<'EOF'
dn: uid=test-user-anna,ou=people,dc=example,dc=org
changetype: add
objectClass: inetOrgPerson
uid: test-user-anna
cn: Anna Newcomer
sn: Newcomer
mail: anna.newcomer@example.org
userPassword: new-anna-password

dn: cn=ductus-viewers,ou=groups,dc=example,dc=org
changetype: modify
add: member
member: uid=test-user-anna,ou=people,dc=example,dc=org
EOF
code=$(signin test-user-anna new-anna-password)
new_id=$(q "SELECT id FROM users WHERE username = 'test-user-anna'")
echo "   the new Anna signs in: $code, account $new_id (the earlier one was $anna_id)"
[ "$code" = 200 ] && [ "$new_id" != "$anna_id" ] || fail+=" reused($code $new_id)"
earlier=$(curl -s -o /dev/null -w '%{http_code}' -H "Cookie: ductus_session=$(cat "$P/earlier-anna.cookie")" "$API/api/v1/session")
aside=$(q "SELECT username || ' ' || (directory_refused_at IS NOT NULL) FROM users WHERE id = $anna_id")
echo "   the earlier Anna's session: $earlier; her account: $aside"
[ "$earlier" = 401 ] || fail+=" earlier-session($earlier)"
[ "$aside" = "test-user-anna#$anna_id true" ] || fail+=" aside($aside)"

echo "== the directory goes down"
bash "$REPO/scripts/dev/slapd.sh" stop "$L"
code=$(signin test-user-ada test-user-ada-password)
echo "   test-user-ada: $code, $(js "d['error']")"
[ "$code" = 503 ] || fail+=" down($code)"
[ "$(as test-user-olaf /session)" = 200 ] || fail+=" open-session-ended"
code=$(signin test-user-breakglass break-glass-password-1)
echo "   test-user-breakglass (local): $code"
[ "$code" = 200 ] || fail+=" break-glass($code)"

actions=$(q "SELECT string_agg(DISTINCT action || ':' || actor_name, ' ' ORDER BY action || ':' || actor_name) FROM audit_log
  WHERE subject IN ($PEOPLE) AND action LIKE 'user.%' AND at >= '$START'")
echo "   audit: $actions"
[[ "$actions" == *"user.create:directory"* && "$actions" == *"user.directory_change:directory"* ]] || fail+=" audit($actions)"
signins=$(q "SELECT count(*) FROM audit_log WHERE action = 'session.sign_in' AND detail->>'with' = 'ldap' AND actor_name LIKE 'test-user-%'
  AND at >= '$START'")
[ "$signins" -ge 5 ] || fail+=" sign-in-rows($signins)"

kill -INT "$ENGINE"
wait "$ENGINE" || true
ENGINE=""
forget
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 "$P/run.log"; exit 1; fi
