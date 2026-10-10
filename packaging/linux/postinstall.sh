#!/bin/sh
# After installing or upgrading the ductus package (deb: `configure [old version]`; rpm: 1 for
# an install, 2 for an upgrade).
set -e

if ! getent group ductus >/dev/null; then
  groupadd --system ductus
fi
if ! getent passwd ductus >/dev/null; then
  useradd --system --gid ductus --home-dir /var/lib/ductus --no-create-home \
    --shell /usr/sbin/nologin --comment "Ductus integration engine" ductus
fi
chown ductus:ductus /var/lib/ductus
chown root:ductus /etc/ductus/ductus.env
chmod 0640 /etc/ductus/ductus.env

upgrade=0
case "$1" in
  configure) [ -n "$2" ] && upgrade=1 ;;
  2) upgrade=1 ;;
esac

if [ -d /run/systemd/system ]; then
  systemctl daemon-reload
  if [ "$upgrade" = 1 ]; then
    # Running: restart on the new version (it stops cleanly first). Stopped: stays stopped.
    systemctl try-restart ductus.service
    exit 0
  fi
fi

if [ "$upgrade" = 0 ]; then
  cat <<'EOF'

Ductus is installed. It isn't running yet: first tell it where its database and project are.
  1. Edit /etc/ductus/ductus.env (the database URL, the project folder, the environment).
  2. Put the project in /var/lib/ductus/project (git clone), readable by the ductus user.
  3. sudo systemctl enable --now ductus
  4. journalctl -u ductus -f      (it says "started" once every interface is up)

EOF
fi
