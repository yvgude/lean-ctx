# P17 TLS test identity

These certificate/key bytes are a deliberately public test identity, signed by a test CA.
Never use this key for deployment or install the certificate into system trust.
Only the individual test client's root store trusts this CA; certificate
verification remains enabled. SANs: `localhost`, `127.0.0.1`; CA: false.

Generated with LibreSSL on 2026-09-08: CA validity 3650 days, server validity
365 days (expires 2027-09-08). Regenerate before server expiration; tests deliberately
do not bypass validity checks. macOS rejects the original ten-year server certificate
as non-compliant; the separate CA and one-year server pass native SSL verification.
Files contain base64-encoded DER CA/server certificates and the unencrypted PKCS#8
server test key. The CA private key is not included.
