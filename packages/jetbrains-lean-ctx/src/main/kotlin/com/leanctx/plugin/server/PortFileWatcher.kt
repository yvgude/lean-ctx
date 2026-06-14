package com.leanctx.plugin.server

import java.io.Closeable
import java.nio.file.FileSystems
import java.nio.file.Path
import java.nio.file.StandardWatchEventKinds
import java.nio.file.WatchKey

/**
 * Watches dataDir for ENTRY_DELETE and invokes onOwnDeleted when the own port file
 * disappears, enabling immediate self-healing re-write (spec §5.4). Owns a single
 * daemon thread; close() shuts the WatchService and ends the thread.
 *
 * Note: an atomic re-write (temp + ATOMIC_MOVE into dataDir) raises CREATE/MODIFY,
 * not DELETE — so re-writing the own file does not re-trigger this watcher.
 */
class PortFileWatcher(
    private val dataDir: Path,
    private val ownPortFile: Path,
    private val onOwnDeleted: () -> Unit,
) : Closeable {
    private val watchService = FileSystems.getDefault().newWatchService()

    @Volatile
    private var running = true
    private val thread: Thread

    init {
        dataDir.register(watchService, StandardWatchEventKinds.ENTRY_DELETE)
        thread = Thread(::runLoop, "leanctx-port-watcher").apply {
            isDaemon = true
            start()
        }
    }

    private fun runLoop() {
        while (running) {
            val key: WatchKey = try {
                watchService.take()
            } catch (_: Exception) {
                return // closed or interrupted
            }
            for (event in key.pollEvents()) {
                val name = event.context() as? Path ?: continue
                if (dataDir.resolve(name) == ownPortFile) {
                    runCatching { onOwnDeleted() }
                }
            }
            if (!key.reset()) return
        }
    }

    override fun close() {
        running = false
        runCatching { watchService.close() }
        thread.interrupt()
    }
}
