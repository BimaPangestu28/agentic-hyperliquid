# Build context: repo root (docker build -f scraper/Dockerfile .)
#
# Base: mcr.microsoft.com/playwright:v1.61.0-jammy — pinned to the installed
# playwright (1.61.1); an older base mismatches the browser binaries.
FROM mcr.microsoft.com/playwright:v1.61.0-jammy
WORKDIR /app

# Real Google Chrome + a virtual display. Cloudflare Turnstile flags both Playwright's
# bundled "Chrome for Testing" and headless mode, so production must run REAL Chrome
# (channel: chrome) HEADFUL under Xvfb. `playwright install chrome` pulls Google Chrome
# stable; xvfb provides the display the headful browser needs on a server.
RUN npx playwright install chrome \
  && apt-get update && apt-get install -y --no-install-recommends xvfb \
  && rm -rf /var/lib/apt/lists/*

COPY scraper/package.json scraper/package-lock.json* ./
RUN npm install --omit=dev && npm install tsx typescript
COPY scraper/ ./

# HEADLESS=false (the app default) + Xvfb. xvfb-run gives Chrome a display :99.
ENV HEADLESS=false
CMD ["xvfb-run", "-a", "--server-args=-screen 0 1280x1024x24", "npx", "tsx", "src/index.ts"]
