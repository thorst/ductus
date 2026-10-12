# The development database for the scripts that need one (check.sh, demos.sh, every demo).
# `source` it; CI and anyone with another server set DUCTUS_DATABASE_URL first. The engine itself
# has no default database (`ductus dev` brings its own).
export DUCTUS_DATABASE_URL=${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
