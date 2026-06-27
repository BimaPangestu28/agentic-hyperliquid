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

# Chrome must render to the Xvfb X11 display, not Wayland. On a Wayland desktop
# WAYLAND_DISPLAY/XDG_SESSION_TYPE are inherited, and Chrome's ozone auto-detect then
# prefers Wayland — ignoring DISPLAY=:99 and hanging at startup ("preparing pages").
# Force X11 so the virtual display is actually used.
unset WAYLAND_DISPLAY
export XDG_SESSION_TYPE=x11

# Reuse an Xvfb already serving this display (a prior `make up` that didn't shut down
# cleanly leaves one running). Starting a second Xvfb on the same display fails with
# "Server is already active for display N" — and because the stale lock/socket still
# exist, a naive socket-existence check would wrongly report "ready", so Chrome attaches
# to a half-dead display and hangs at startup. Decide on the live PROCESS, not the socket
# file, and clear any stale lock + socket before starting fresh.
if pgrep -f "Xvfb ${DISPLAY}" >/dev/null 2>&1; then
  echo "entrypoint: reusing Xvfb already running on $DISPLAY — starting scraper"
else
  rm -f "/tmp/.X${DISPLAY_NUM}-lock" "/tmp/.X11-unix/X${DISPLAY_NUM}"
  # 1920x1080 matches the Chrome window/viewport in index.ts so chart screenshots aren't
  # clipped by a smaller virtual screen.
  Xvfb "$DISPLAY" -screen 0 1920x1080x24 -nolisten tcp &
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
fi

exec node --env-file-if-exists=.env --import tsx src/index.ts
