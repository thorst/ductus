#!/bin/sh
# After removing the ductus package. The ductus user, /var/lib/ductus (the project) and
# /etc/ductus/ductus.env stay: reinstalling picks up where it left off. The database is the DBA's.
set -e

if [ -d /run/systemd/system ]; then
  systemctl daemon-reload || true
fi
