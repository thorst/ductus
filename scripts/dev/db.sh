# The development database for the scripts that need one (check.sh, demos.sh, every demo).
# `source` it. CI and anyone with another server set DUCTUS_DATABASE_URL first; a personal setup
# goes in scripts/dev/db.local.sh (not in Git), e.g.
#     export DUCTUS_DATABASE_URL=postgres://me:secret@localhost/ductus_dev
#     export PGUSER_=me PGPASS_=secret       # the same login, for the demos that configure a resource
# The engine itself has no default database (`ductus dev` brings its own).
[ -f "$(dirname "${BASH_SOURCE[0]}")/db.local.sh" ] && source "$(dirname "${BASH_SOURCE[0]}")/db.local.sh"
export DUCTUS_DATABASE_URL=${DUCTUS_DATABASE_URL:-postgres://ductus:ductus@localhost/ductus_dev}
