# Secure engineering review notes
## Review item A
RISK-147 — The patch endpoint builds `format!("apply {}", request.patch)` and passes it to `sh -c`; `request.patch` is client controlled.

## Review item B
RISK-153 — The archive reader joins `workspace_root` with the member name and opens it without rejecting `..` components or canonicalizing the result.

## Review item C
RISK-161 — A checked relative path may resolve through a symlink to a file outside the workspace after the prefix check.

## Review item D
RISK-166 — Debug logging serializes the full Authorization header when an upstream request fails.

## Review item E
RISK-173 — The URL fetcher accepts an arbitrary host from the request body and follows redirects to private address ranges.

## Review item F
RISK-179 — The outbound client installs a verifier that accepts every certificate in production builds.

## Review item G
RISK-184 — The service checks file ownership, closes the handle, then reopens the path for writing.

## Review item H
RISK-191 — The handler loads tenant data and formats a response before checking whether the caller belongs to that tenant.

## Review item I
RISK-197 — A client-supplied binary configuration is deserialized directly into an internally tagged enum before size and variant checks.

## Review item J
RISK-203 — The anonymous preview endpoint can trigger a full repository scan and has no per-client or global concurrency limit.

## Review item K
RISK-211 — The process launcher forwards the complete service environment, including signing credentials, to an untrusted hook.

## Review item L
RISK-218 — The watcher replaces the active policy before parsing and validating every rule in the new file.
