"""Exception hierarchy for the lean-ctx SDK."""


class LeanCtxError(Exception):
    """Base class for every error raised by the lean-ctx SDK."""


class LeanCtxConnectionError(LeanCtxError):
    """The local lean-ctx proxy could not be reached.

    Usually means the daemon is not running (``lean-ctx proxy enable``) or the
    discovered host/port is wrong (override with ``LEAN_CTX_PROXY_URL``).
    """


class LeanCtxAuthError(LeanCtxError):
    """The proxy rejected the request (missing or invalid session token).

    Provide the token explicitly, or export ``LEAN_CTX_PROXY_TOKEN`` so the SDK
    can authenticate against the loopback proxy.
    """
