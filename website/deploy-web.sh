#!/usr/bin/env bash
# deploy-web.sh — build + (re)deploy the leanctx.com static website container.
#
# Mirrors the cloud-api / billing deploys: idempotent, image backup, health-gated
# rollback. Run this ON the Docker host (pounce-server) from the Astro project
# root (the dir that holds package.json + Dockerfile; the Dockerfile does
# `COPY . .` and builds the static site, then serves it from nginx).
#
# The site is fronted by Traefik's file provider (service `leanctx-web` ->
# http://leanctx-web:80 on the `coolify` network), so the container only needs
# its name + network; no env, ports or labels are required.
#
# Usage:
#   ./deploy-web.sh
set -euo pipefail

NAME="leanctx-web"
IMAGE="lean-ctx-web:latest"
BACKUP_IMAGE="lean-ctx-web:backup"
NETWORK="coolify"
PORT="80"
DOCKERFILE="Dockerfile"
CURL_IMAGE="curlimages/curl:latest"

cd "$(dirname "$0")"

log() { printf '\033[36m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

[ -f "$DOCKERFILE" ] || die "missing $DOCKERFILE (run from the Astro project root)"
[ -f package.json ] || die "missing package.json (run from the Astro project root)"

# ── 1. Build the new image (runs `npm run build` inside the builder stage) ─────
log "building $IMAGE:new from $DOCKERFILE"
docker build -f "$DOCKERFILE" -t "${IMAGE%:*}:new" .

# ── 2. Back up the currently-deployed image, then promote the new one ─────────
if docker image inspect "$IMAGE" >/dev/null 2>&1; then
  docker tag "$IMAGE" "$BACKUP_IMAGE"
  log "backed up current image -> $BACKUP_IMAGE"
fi
docker tag "${IMAGE%:*}:new" "$IMAGE"
docker rmi "${IMAGE%:*}:new" >/dev/null 2>&1 || true

# ── 3. Swap the container ─────────────────────────────────────────────────────
log "replacing container $NAME"
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d \
  --name "$NAME" \
  --network "$NETWORK" \
  --restart unless-stopped \
  "$IMAGE" >/dev/null

# ── 4. Health-gate (rollback on failure) ──────────────────────────────────────
# Reach the new container by name on the shared docker network via a throwaway
# curl container — nginx is not host-published (Traefik fronts it).
probe() {
  docker run --rm --network "$NETWORK" "$CURL_IMAGE" \
    -fsS --max-time 4 "http://${NAME}:${PORT}$1" 2>/dev/null
}

log "waiting for / …"
healthy=0
for _ in $(seq 1 20); do
  if probe / >/dev/null; then healthy=1; break; fi
  sleep 1
done

if [ "$healthy" -eq 1 ]; then
  log "healthy: GET / OK"
  docker rmi "$BACKUP_IMAGE" >/dev/null 2>&1 || true
  log "DONE — $NAME is live on $IMAGE"
else
  printf '\033[31m==> health check FAILED — rolling back\033[0m\n' >&2
  docker logs --tail 40 "$NAME" 2>&1 | sed 's/^/    /' >&2 || true
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  if docker image inspect "$BACKUP_IMAGE" >/dev/null 2>&1; then
    docker tag "$BACKUP_IMAGE" "$IMAGE"
    docker run -d --name "$NAME" --network "$NETWORK" --restart unless-stopped "$IMAGE" >/dev/null
    log "rolled back to previous image ($BACKUP_IMAGE)"
  fi
  die "deploy aborted; previous version restored"
fi
