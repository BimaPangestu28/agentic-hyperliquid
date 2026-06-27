# Convenience targets for running the scraper + the SSH tunnel to the bot API.
# VPS_HOST and BOT_CLUSTER_IP are read from .env (gitignored) so no infra addresses
# live in the repo. See .env.example for the keys.

LOCAL_PORT ?= 8088
VPS_HOST       := $(shell grep -E '^VPS_HOST=' .env 2>/dev/null | cut -d= -f2-)
BOT_CLUSTER_IP := $(shell grep -E '^BOT_CLUSTER_IP=' .env 2>/dev/null | cut -d= -f2-)
SSH_OPTS := -o ServerAliveInterval=30 -o ServerAliveCountMax=3 -o ExitOnForwardFailure=yes -o StrictHostKeyChecking=accept-new

.PHONY: help login tunnel start start-xvfb up check check-env

help:
	@echo "Targets:"
	@echo "  make login       - log in to Neurobro once (visible Chrome, solve Cloudflare)"
	@echo "  make tunnel      - open the SSH tunnel to the bot API (foreground)"
	@echo "  make start       - run the scraper, VISIBLE Chrome window"
	@echo "  make start-xvfb  - run the scraper INVISIBLE under Xvfb (real Chrome, no window)"
	@echo "  make up          - tunnel + scraper (Xvfb) together; Ctrl-C stops both"
	@echo "  make check       - verify the tunnel reaches the bot API"

check-env:
	@test -n "$(VPS_HOST)" || { echo "ERROR: VPS_HOST not set in .env"; exit 1; }
	@test -n "$(BOT_CLUSTER_IP)" || { echo "ERROR: BOT_CLUSTER_IP not set in .env"; exit 1; }

login:
	npm run login

# Foreground SSH tunnel: localhost:$(LOCAL_PORT) -> VPS -> bot ClusterIP:8088
tunnel: check-env
	ssh -N $(SSH_OPTS) -L $(LOCAL_PORT):$(BOT_CLUSTER_IP):8088 $(VPS_HOST)

# Visible Chrome (good for debugging / watching).
start:
	npm run start

# Invisible: real headful Chrome on a virtual display (:99) via Xvfb. Needs `xvfb`
# installed. Passes Cloudflare (real Chrome) without opening a window — the "headless"
# you want. True HEADLESS=true is NOT recommended (Cloudflare flags it).
start-xvfb:
	sh entrypoint.sh

# Tunnel (background) + scraper (Xvfb) in one command. Ctrl-C stops both.
up: check-env
	@echo "Starting tunnel + scraper (Xvfb). Ctrl-C stops both."
	@ssh -N $(SSH_OPTS) -L $(LOCAL_PORT):$(BOT_CLUSTER_IP):8088 $(VPS_HOST) & \
	TUN=$$!; trap 'kill $$TUN 2>/dev/null' EXIT INT TERM; \
	sleep 3; sh entrypoint.sh

# Confirm the tunnel is up and the bot API answers (uses BOT_API_TOKEN from .env).
check:
	@curl -fsS -m 8 -H "Authorization: Bearer $$(grep -E '^BOT_API_TOKEN=' .env | cut -d= -f2-)" \
		http://127.0.0.1:$(LOCAL_PORT)/watchlist && echo "  <- bot API reachable" \
		|| echo "bot API NOT reachable (is the tunnel up?)"
