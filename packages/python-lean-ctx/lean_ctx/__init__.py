"""lean-ctx SDK — context compression for AI agents and frameworks.

Drop-in usage::

    from lean_ctx import compress
    messages = compress(messages, model="claude-sonnet-4")
"""

from lean_ctx.client import LeanCtxClient
from lean_ctx.errors import LeanCtxAuthError, LeanCtxConnectionError, LeanCtxError
from lean_ctx.langchain import LeanCtxRetriever, compress_messages
from lean_ctx.litellm import LeanCtxLiteLLMHandler, compress_request_data
from lean_ctx.llamaindex import LeanCtxNodeParser
from lean_ctx.proxy import CompressResult, ProxyClient, compress

__version__ = "0.3.0"
__all__ = [
    "compress",
    "ProxyClient",
    "CompressResult",
    "LeanCtxClient",
    "LeanCtxRetriever",
    "compress_messages",
    "LeanCtxLiteLLMHandler",
    "compress_request_data",
    "LeanCtxNodeParser",
    "LeanCtxError",
    "LeanCtxConnectionError",
    "LeanCtxAuthError",
]
