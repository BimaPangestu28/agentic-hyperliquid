#!/bin/sh
# Start a virtual display, wait until it is actually ready, then run the scraper.
#
# This replaces `xvfb-run`, whose SIGUSR1 "display ready" handshake races at container
# startup: Xvfb starts but xvfb-run can block forever in sigsuspend waiting for the
# signal, so the command (node) never execs. Starting Xvfb ourselves and polling for the
# X socket is deterministic.
set -e

: "${DISPLAY_NUM:=99}"
export DISPLAY=":${DISPLAY_NUM}"

Xvfb "$DISPLAY" -screen 0 1280x1024x24 -nolisten tcp &
XVFB_PID=$!

# Wait up to ~10s for the X socket to appear before launching Chrome.
attempt=0
while [ ! -e "/tmp/.X11-unix/X${DISPLAY_NUM}" ]; do
  attempt=$((attempt + 1))
  if [ "$attempt" -gt 50 ]; then
    echo "entrypoint: Xvfb failed to become ready on $DISPLAY" >&2
    exit 1
  fi
  sleep 0.2
done
echo "entrypoint: Xvfb ready on $DISPLAY (pid $XVFB_PID) — starting scraper"

exec node --import tsx src/index.ts
