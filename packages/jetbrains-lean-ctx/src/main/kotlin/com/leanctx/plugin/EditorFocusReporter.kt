package com.leanctx.plugin

import com.intellij.openapi.Disposable
import com.intellij.openapi.util.registry.Registry
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.util.Alarm
import com.intellij.util.concurrency.AppExecutorUtil
import java.util.concurrent.TimeUnit

/**
 * Editor focus signal (#500), JetBrains producer side. Reports the focused file
 * path to lean-ctx via `lean-ctx editor-signal --file <path>` so the context
 * engine ranks it up. Paths only — never content — and only files inside the
 * current project. 1:1 parity with vscode-extension/src/editor-signal.ts.
 *
 * The core (isUnderBasePath / maybeReport) operates on primitives so it is unit
 * testable without an IDE platform driver. The VirtualFile adapter, the spawn,
 * and the debounce Alarm are platform-bound and covered by the manual runIde gate.
 *
 * @param parentDisposable project-scoped disposable; the debounce Alarm is bound
 *   to it so it is cancelled on project close (no leak, no spawn after close).
 * @param basePath the project base path; files outside it are not reported.
 * @param isEnabled producer-side opt-out gate (registry key, evaluated per event).
 * @param spawn fire-and-forget binary call; injectable for tests.
 * @param schedule debounce scheduler; null uses a 2s POOLED_THREAD Alarm.
 *   Injectable for tests (e.g. synchronous `{ it() }`).
 */
class EditorFocusReporter(
    parentDisposable: Disposable,
    private val basePath: String?,
    private val isEnabled: () -> Boolean = { Registry.`is`("leanctx.editor.signal.enabled", true) },
    private val spawn: (String) -> Unit = ::defaultSpawn,
    schedule: ((() -> Unit) -> Unit)? = null,
) {
    private var lastSent: String? = null

    /** Debounce: cancel any pending request and (re)schedule, collapsing rapid tab hops. */
    private val schedule: (() -> Unit) -> Unit = schedule ?: run {
        val alarm = Alarm(Alarm.ThreadToUse.POOLED_THREAD, parentDisposable)
        val scheduler: (() -> Unit) -> Unit = { action ->
            alarm.cancelAllRequests()
            alarm.addRequest(action, DEBOUNCE_MS)
        }
        scheduler
    }

    /** Thin platform adapter: extract primitives from the VirtualFile, then delegate. */
    fun onFileFocused(file: VirtualFile?) {
        if (file == null) return
        maybeReport(file.isInLocalFileSystem, file.isDirectory, file.path)
    }

    /**
     * Core decision, testable without a VirtualFile/Registry/Alarm:
     * registry gate → real-local-project-file filter → path dedup → debounced spawn.
     */
    internal fun maybeReport(isLocal: Boolean, isDirectory: Boolean, path: String) {
        if (!isEnabled()) return
        if (!isLocal || isDirectory) return
        if (!isUnderBasePath(path, basePath)) return
        // Dedup before debounce: same path back-to-back schedules at most one spawn.
        if (path == lastSent) return
        lastSent = path
        schedule { spawn(path) }
    }

    companion object {
        /** Debounce window, identical to VS Code's DEBOUNCE_MS. */
        const val DEBOUNCE_MS = 2_000

        /**
         * True iff [path] is [basePath] itself or sits under it on a path
         * boundary. VS Code uses a plain startsWith; we additionally require a
         * '/' segment boundary so /foo/bar2 is not treated as under /foo/bar.
         */
        fun isUnderBasePath(path: String, basePath: String?): Boolean {
            if (basePath.isNullOrEmpty()) return false
            return path == basePath || path.startsWith("$basePath/")
        }
    }
}

/**
 * Fire-and-forget producer call. Runs on a pooled thread (never the EDT). A lost
 * signal is harmless: the next tab change resends. A binary that is missing or
 * too old (no `editor-signal` subcommand) is swallowed silently, mirroring VS
 * Code's `.catch()`. We waitFor with a short timeout only to reap the short-lived
 * child, never to block UI.
 */
private fun defaultSpawn(path: String) {
    val binary = BinaryResolver.resolve() ?: return
    AppExecutorUtil.getAppExecutorService().execute {
        try {
            val process = ProcessBuilder(binary, "editor-signal", "--file", path)
                .redirectErrorStream(true)
                .start()
            process.waitFor(5, TimeUnit.SECONDS)
        } catch (_: Exception) {
            // missing/old binary or IO error — a lost signal is harmless
        }
    }
}
