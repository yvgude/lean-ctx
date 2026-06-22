"""LangChain integration for lean-ctx."""

from typing import Any, List, Optional

from lean_ctx.client import LeanCtxClient
from lean_ctx.proxy import compress as _compress_dicts


def _reattach_content(originals: List[Any], compressed: List[Any]) -> List[Any]:
    """Return clones of ``originals`` with ``content`` taken from ``compressed``.

    Pydantic ``model_copy`` preserves each message's type and metadata; only the
    textual content is swapped. If the conversion changed the message count (e.g.
    tool-call splitting) the mapping is ambiguous, so the originals are returned
    unchanged rather than risk corrupting the transcript.
    """
    if len(originals) != len(compressed):
        return list(originals)
    out: List[Any] = []
    for original, comp in zip(originals, compressed):
        new_content = comp.get("content") if isinstance(comp, dict) else None
        if new_content is None or not hasattr(original, "model_copy"):
            out.append(original)
        else:
            out.append(original.model_copy(update={"content": new_content}))
    return out


def compress_messages(messages: List[Any], model: Optional[str] = None, **kwargs: Any) -> List[Any]:
    """Compress the textual content of a list of LangChain ``BaseMessage`` objects.

    Converts to the OpenAI wire shape, runs the deterministic proxy compression,
    and returns new messages with only their ``content`` rewritten. Requires
    ``langchain-core``. Extra keyword arguments (``base_url``, ``token``,
    ``timeout``) are forwarded to :func:`lean_ctx.compress`.
    """
    try:
        from langchain_core.messages import convert_to_openai_messages
    except ImportError as exc:  # pragma: no cover - optional dependency
        raise ImportError("langchain-core is required: pip install langchain-core") from exc

    openai_messages = convert_to_openai_messages(messages)
    compressed = _compress_dicts(openai_messages, model, **kwargs)
    return _reattach_content(messages, compressed)

try:
    from langchain_core.retrievers import BaseRetriever
    from langchain_core.documents import Document
    from langchain_core.callbacks import CallbackManagerForRetrieverRun

    class LeanCtxRetriever(BaseRetriever):
        """LangChain retriever backed by lean-ctx hybrid search."""

        client: LeanCtxClient = None
        top_k: int = 10

        def __init__(self, project_root: Optional[str] = None, top_k: int = 10, **kwargs):
            super().__init__(**kwargs)
            self.client = LeanCtxClient(project_root=project_root)
            self.top_k = top_k

        def _get_relevant_documents(
            self, query: str, *, run_manager: CallbackManagerForRetrieverRun
        ) -> list[Document]:
            result = self.client.search(query)
            documents = []
            for line in result.split("\n"):
                if not line.strip():
                    continue
                parts = line.split(":", 2)
                if len(parts) >= 3:
                    file_path, line_num, content = parts[0], parts[1], parts[2]
                    documents.append(
                        Document(
                            page_content=content.strip(),
                            metadata={"source": file_path, "line": line_num},
                        )
                    )
                else:
                    documents.append(Document(page_content=line.strip()))

            return documents[: self.top_k]

except ImportError:

    class LeanCtxRetriever:
        """Stub: install langchain-core for full integration."""

        def __init__(self, **kwargs):
            raise ImportError(
                "langchain-core is required: pip install langchain-core"
            )
