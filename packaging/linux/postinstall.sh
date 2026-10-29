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
    if systemctl is-active --quiet ductus.service && [ -S /run/ductus/control ]; then
      # Running: the new version takes over beside it, nothing refused. If it can't start, the
      # running version goes on: the package is installed, the engine isn't upgraded yet.
      if ! systemctl reload ductus.service; then
        echo "ductus: the new version didn't take over; the running one goes on. Why: journalctl -u ductus" >&2
        echo "ductus: fix it and run 'systemctl reload ductus' (or 'systemctl restart ductus')." >&2
      fi
    else
      # Running a version without the handover (before 0.6.6.3): restart on the new one (it
      # stops cleanly first; systemd keeps the listening sockets). Stopped: stays stopped.
      systemctl try-restart ductus.service
    fi
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
