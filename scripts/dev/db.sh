# The development database for the scripts that need one (check.sh, demos.sh, every demo).
# `source` it. CI and anyone with another server set DUCTUS_DATABASE_URL first; a personal setup
# goes in scripts/dev/db.local.sh (not in Git), e.g.
#     export DUCTUS_DATABASE_URL=postgres://me:secret@localhost/ductus_dev
#     export PGUSER_=me PGPASS_=secret       # the same login, for the demos that configure a resource
# The engine itself has no default database (`ductus dev` brings its own).
[ -f "$(dirname "${BASH_SOURCE[0]}")/db.local.sh" ] && source "$(dirname "${BASH_SOURCE[0]}")/db.local.sh"
export DUCTUS_DATABASE_URL=${DUCTUS_DATABASE_URL:-postgres://ductus:ductus@localhost/ductus_dev}

# The URL's parts, for the demos that configure a database resource on the same server (CI's is
# `ductus dev`'s: its own port and login): PGUSER_ and PGPASS_ (a login set before wins), PGHOST_,
# PGPORT_, PGDATABASE_. What the URL leaves out is libpq's default.
url_parts='^postgres(ql)?://(([^:@/]*)(:([^@/]*))?@)?([^:/?]*)(:([0-9]+))?/([^?]*)'
if [[ $DUCTUS_DATABASE_URL =~ $url_parts ]]; then
  export PGUSER_=${PGUSER_:-${BASH_REMATCH[3]:-$(id -un)}} PGPASS_=${PGPASS_:-${BASH_REMATCH[5]}}
  export PGHOST_=${BASH_REMATCH[6]:-localhost} PGPORT_=${BASH_REMATCH[8]:-5432} PGDATABASE_=${BASH_REMATCH[9]}
fi
unset url_parts

# forget_test_users PATTERN...: delete test accounts (LIKE patterns; a plain name matches itself;
# each must start with test-user-) and what refers to them, in one transaction. Accounts are never
# deleted in the product, so every test and demo deletes the ones it made, or a long-lived database
# fills with them. Actions keep their row and actor name with the account cleared; audit rows stay.
# The same statements as `ductus_store::testing` (the Rust tests' helper).
forget_test_users() {
  local p list=""
  for p in "$@"; do
    case $p in
      test-user-*) list+="${list:+,}'$p','$p#%'" ;; # and the account a reused name moved aside
      *) echo "forget_test_users: $p isn't a test account's name" >&2; return 1 ;;
    esac
  done
  [ -n "$list" ] || return 0
  local users="SELECT id FROM users WHERE username LIKE ANY (ARRAY[$list])"
  local tokens="SELECT id FROM api_tokens WHERE user_id IN ($users)"
  psql -q -1 "$DUCTUS_DATABASE_URL" \
    -c "DELETE FROM api_sessions WHERE user_id IN ($users) OR token_id IN ($tokens)" \
    -c "UPDATE message_actions SET user_id = NULL WHERE user_id IN ($users)" \
    -c "UPDATE message_actions SET token_id = NULL WHERE token_id IN ($tokens)" \
    -c "DELETE FROM api_tokens WHERE user_id IN ($users)" \
    -c "DELETE FROM user_settings WHERE user_id IN ($users)" \
    -c "DELETE FROM users WHERE id IN ($users)"
}

# forget_tokens NAME...: revokes the tokens a test or demo made without a person (`ductus token
# create`), by name; `%` matches any run's (`'ui-demo-%'`). Call it in the script's EXIT trap, not
# at its end: a script that ends early otherwise leaves them, and the Tokens page lists every one
# (243 left by demo-ui alone made its browser test overrun, 2026-11-17). A name must have at least
# four characters before any `%`, so no pattern can revoke everything.
forget_tokens() {
  local p list=""
  for p in "$@"; do
    case ${p%%\%*} in
      ????*) list+="${list:+,}'$p'" ;;
      *) echo "forget_tokens: '$p' would match too much" >&2; return 1 ;;
    esac
  done
  [ -n "$list" ] || return 0
  psql -q "$DUCTUS_DATABASE_URL" -c "UPDATE api_tokens SET revoked_at = now() WHERE revoked_at IS NULL AND user_id IS NULL AND name LIKE ANY (ARRAY[$list])"
}
