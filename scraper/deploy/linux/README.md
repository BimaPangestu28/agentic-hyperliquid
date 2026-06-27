# Running the Neurobro scraper on a Linux laptop (24/7)

The scraper must run from a **residential IP** (Cloudflare/Neurobro blocks datacenter
IPs). A Linux laptop at home works. It connects to the bot's HTTP API on the VPS through
an SSH tunnel, so the bot API is never exposed publicly.

> ⚠️ **Only ONE scraper may run at a time.** Stop any scraper on another machine first —
> two running scrapers double-trade.

## 1. Prerequisites

```bash
# Node >= 20 (via nvm or nodesource), git, real Google Chrome, and Xvfb.
sudo apt update
sudo apt install -y google-chrome-stable xvfb
# Node: e.g. nvm install 20   (or use your distro's nodesource setup)
```

## 2. Code + dependencies

```bash
git clone git@github.com:BimaPangestu28/agentic-hyperliquid.git
cd agentic-hyperliquid/scraper
npm install
npx playwright install chrome        # real Chrome (beats Cloudflare); Xvfb gives it a display
npx playwright install-deps          # system libraries Chrome needs
```

## 3. Configuration (.env)

Copy the working `.env` from the machine where it already runs (it has the bot token,
Telegram token, HL_TIMEFRAME=1D, etc.). From that machine:

```bash
scp scraper/.env  <user>@<linux-ip>:~/agentic-hyperliquid/scraper/.env
```

`BOT_API_URL` stays `http://127.0.0.1:8088` — the tunnel (step 5) maps it to the VPS bot.

## 4. Log in to Neurobro (one time, on the real display)

```bash
npm run login
```

A Chrome window opens. Solve the Cloudflare check, log in to Neurobro, wait for the chat
composer, then press Enter in the terminal. The session is saved to `./neurobro-profile`.
Re-run this whenever the scraper alerts that the Neurobro session expired.

## 5. Passwordless SSH to the VPS (for the tunnel service)

Replace `VPS_HOST` with your own `user@host` (e.g. `root@your.vps.ip`).

```bash
ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519 -N ""   # skip if you already have a key
ssh-copy-id VPS_HOST                                 # enter the VPS password once
ssh VPS_HOST true                                    # verify: should NOT prompt for a password
```

## 6. Install the systemd user services (auto-start + auto-restart)

First edit `bot-tunnel.service` and replace the two placeholders with your values:

```bash
# VPS_HOST = your user@host; BOT_CLUSTER_IP from:
ssh VPS_HOST "kubectl -n agentic-hyperliquid get svc bot -o jsonpath='{.spec.clusterIP}'"
# then edit deploy/linux/bot-tunnel.service and substitute BOT_CLUSTER_IP and VPS_HOST.
```

```bash
mkdir -p ~/.config/systemd/user
cp deploy/linux/bot-tunnel.service       ~/.config/systemd/user/
cp deploy/linux/neurobro-scraper.service ~/.config/systemd/user/

systemctl --user daemon-reload
systemctl --user enable --now bot-tunnel.service
systemctl --user enable --now neurobro-scraper.service

# Keep services running after you log out / on boot (so it survives without an active session):
sudo loginctl enable-linger "$USER"
```

Check status / logs:

```bash
systemctl --user status neurobro-scraper.service
journalctl --user -u neurobro-scraper.service -f      # live scraper logs
journalctl --user -u bot-tunnel.service -f            # tunnel logs
```

## 7. Verify

- Telegram should show `🚀 Scraper online`, then `🔄 Scan ...` each cycle.
- The bot API is reachable through the tunnel:
  ```bash
  curl -H "Authorization: Bearer $(grep '^BOT_API_TOKEN=' .env | cut -d= -f2-)" http://127.0.0.1:8088/watchlist
  ```

## Notes

- Laptop must stay **on and not suspended** for 24/7. Disable sleep, or run on AC with
  suspend disabled (`systemd-inhibit` / power settings).
- The scraper runs headful Chrome on Xvfb (`:99`) — invisible; watch it via Telegram.
- The bot ClusterIP you put in `bot-tunnel.service` is stable unless the bot Service is
  recreated; if the tunnel stops connecting, re-fetch it (command in that file).
- To stop trading: `systemctl --user stop neurobro-scraper.service` (or `/set
  auto_scalp_enabled off` in Telegram — the scraper then idles).
