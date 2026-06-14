package com.leanctx.plugin.server

import org.junit.Assert.assertTrue
import org.junit.Test
import java.nio.file.Files
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class PortFileWatcherTest {
    @Test
    fun firesOnOwnFileDelete() {
        val dir = Files.createTempDirectory("lc-watch")
        val own = dir.resolve("jetbrains-own.port")
        Files.writeString(own, "{}")
        val latch = CountDownLatch(1)

        val watcher = PortFileWatcher(dir, own) { latch.countDown() }
        try {
            // Give the watch thread a moment to register before mutating.
            Thread.sleep(200)
            Files.delete(own)
            assertTrue(
                "onOwnDeleted must fire within timeout",
                latch.await(10, TimeUnit.SECONDS)
            )
        } finally {
            watcher.close()
        }
    }
}
