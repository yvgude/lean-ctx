# LeanCTX Cloud Server

## SSH Access
- Host: `pounce-server` (185.142.213.170)
- User: `administrator`
- Key: `~/.ssh/pounce_server`

## Cloud API Container
- Name: `lean-ctx-cloud-api`
- Image: `lean-ctx-cloud-api:latest`
- Network: `coolify`
- Build: `docker build -f cloud-infra/Dockerfile.cloud-api -t lean-ctx-cloud-api:latest .`
- Source on server: `/home/administrator/lean-ctx-cloud/`

## Database
- Container: `lean-ctx-cloud-db`
- PostgreSQL
- Connection: `postgres://leanctx_cloud:b3bdeGxVSGo1E6f1q5NYAfIOR8xzhb9edprxSWW9@lean-ctx-cloud-db:5432/leanctx_cloud`

## SMTP (ZeptoMail)
- Host: `smtp.zeptomail.eu`
- Port: 587 (TLS) / 465 (SSL)
- TLS: TLSv1.2 only
- Username: `emailapikey`
- Password: `yA6KbHtY4wv1kGkDFEZo08CPoYplraltiC7isXrjLswkftC33KE/gRJqdNC/I2bZitTXtahRadpEdYnrtt1ZLZgxNIJYLZTGTuv4P2uV48xh8ciEYNYjhJytBbASFaNKeRohCS0zQ/UhWA==`
- From: `noreply@leanctx.com`
- Domain: `leanctx.com`

## Environment Variables
- `LEANCTX_CLOUD_BIND_HOST=0.0.0.0`
- `LEANCTX_CLOUD_BIND_PORT=8088`
- `LEANCTX_CLOUD_PUBLIC_BASE_URL=https://leanctx.com`
- `LEANCTX_CLOUD_API_BASE_URL=https://api.leanctx.com`
- `LEANCTX_CLOUD_JWT_SECRET=wrv7GaBA781ZTd4QIyugnSxcLNMGbXZfYIYTUxkxMfyMzLbMJigPnySFDCmf8UW`

## Traefik (Proxy)
- Config: `/traefik/dynamic/leanctx-com.yml` inside `coolify-proxy` container
- Routes: `leanctx.com` -> `leanctx-web:80`, `api.leanctx.com` -> `lean-ctx-cloud-api:8088`

## Notes
- ZeptoMail Agent: "leanctx", created 13 Apr 2026
- ZeptoMail requires TLSv1.2 only
- Account status: "Ihr Konto wird in Kürze geprüft" (account under review)
