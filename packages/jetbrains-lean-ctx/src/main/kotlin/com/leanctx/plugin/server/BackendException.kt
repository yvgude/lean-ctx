package com.leanctx.plugin.server

/** Carries a wire error `code` (spec §6) for a fachlicher Negativfall (HTTP 200). */
class BackendException(val code: String, message: String) : RuntimeException(message)
