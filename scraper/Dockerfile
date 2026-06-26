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

# HEADLESS=false (the app default) + Xvfb. entrypoint.sh starts Xvfb on :99, waits for
# it to be ready, then execs node — deterministic, unlike xvfb-run's flaky USR1 wait.
ENV HEADLESS=false
RUN chmod +x entrypoint.sh
CMD ["sh", "entrypoint.sh"]
