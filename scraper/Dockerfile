# Build context: repo root (docker build -f scraper/Dockerfile .)
#
# Base image: mcr.microsoft.com/playwright:v1.61.0-jammy
# Pinned to match the npm package installed version (1.61.1).
# The brief template referenced v1.48.0-jammy, but the lockfile resolves
# playwright@^1.48.0 to 1.61.1 — using a base image older than the npm
# package causes browser-binary mismatch on first launch.
FROM mcr.microsoft.com/playwright:v1.61.0-jammy
WORKDIR /app
COPY scraper/package.json scraper/package-lock.json* ./
RUN npm install --omit=dev && npm install tsx typescript
COPY scraper/ ./
CMD ["npx", "tsx", "src/index.ts"]
