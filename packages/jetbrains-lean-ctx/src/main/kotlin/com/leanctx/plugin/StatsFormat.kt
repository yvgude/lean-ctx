package com.leanctx.plugin

import java.util.Locale

/** Human token count: 1_234_567 → "1.2M", 4_321 → "4.3K", 42 → "42". Always Locale.US. */
fun formatTokens(n: Long): String = when {
    n >= 1_000_000 -> "%.1fM".format(Locale.US, n / 1_000_000.0)
    n >= 1_000 -> "%.1fK".format(Locale.US, n / 1_000.0)
    else -> n.toString()
}
