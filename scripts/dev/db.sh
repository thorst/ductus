# The development database for the scripts that need one (check.sh, demos.sh, every demo).
# `source` it. CI and anyone with another server set DUCTUS_DATABASE_URL first; a personal setup
# goes in scripts/dev/db.local.sh (not in Git), e.g.
#     export DUCTUS_DATABASE_URL=postgres://me:secret@localhost/ductus_dev
#     export PGUSER_=me PGPASS_=secret       # the same login, for the demos that configure a resource
# The engine itself has no default database (`ductus dev` brings its own).
[ -f "$(dirname "${BASH_SOURCE[0]}")/db.local.sh" ] && source "$(dirname "${BASH_SOURCE[0]}")/db.local.sh"
export DUCTUS_DATABASE_URL=${DUCTUS_DATABASE_URL:-postgres://ductus:ductus@localhost/ductus_dev}

# forget_test_users PATTERN...: delete test accounts (LIKE patterns; a plain name matches itself;
# each must start with test-user-) and what refers to them, in one transaction. Accounts are never
# deleted in the product, so every test and demo deletes the ones it made, or a long-lived database
# fills with them. Actions keep their row and actor name with the account cleared; audit rows stay.
# The same statements as `ductus_store::testing` (the Rust tests' helper).
forget_test_users() {
  local p list=""
  for p in "$@"; do
    case $p in
      test-user-*) list+="${list:+,}'$p'" ;;
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
