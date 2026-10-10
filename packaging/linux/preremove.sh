#!/bin/sh
# Before removing the ductus package (deb: `remove` or `upgrade`; rpm: 0 when removed, 1 when
# upgraded). Only a removal stops the service: an upgrade restarts it afterwards.
set -e

case "$1" in
  remove | 0)
    if [ -d /run/systemd/system ]; then
      systemctl disable --now ductus.service || true
    fi
    ;;
esac
