package com.leanctx.plugin.util

// ESC [ ... <final byte> — matches ANSI CSI sequences (colour/SGR etc.) that
// Swing dialogs cannot render. The CLI emits these (e.g. `lean-ctx doctor`);
// strip them before showing captured output in a Messages popup.
private val ANSI_CSI = Regex("\\[[0-9;?]*[ -/]*[@-~]")

internal fun stripAnsi(text: String): String = ANSI_CSI.replace(text, "")
