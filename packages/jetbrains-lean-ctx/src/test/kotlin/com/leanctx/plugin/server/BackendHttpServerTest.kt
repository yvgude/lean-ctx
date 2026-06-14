package com.leanctx.plugin.server

import com.intellij.testFramework.fixtures.BasePlatformTestCase
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.nio.file.Files
import java.nio.file.Path

class BackendHttpServerTest : BasePlatformTestCase() {
    private fun get(port: Int, token: String?): HttpResponse<String> {
        val b = HttpRequest.newBuilder(URI.create("http://127.0.0.1:$port/health")).GET()
        if (token != null) b.header("X-LeanCtx-Token", token)
        return HttpClient.newHttpClient().send(b.build(), HttpResponse.BodyHandlers.ofString())
    }

    fun testStartWritesPortFileAndServesHealth() {
        val dataDir = Files.createTempDirectory("lc-srv")
        val server = BackendHttpServer(
            dataDir = dataDir, project = project, projectRoot = "/some/project",
            ideVersion = "IC-2026.1.3", projectName = "demo", startedAt = 1L
        )
        try {
            server.start()
            val portFile = LeanCtxPaths.portFile(dataDir, "/some/project")
            assertTrue(Files.exists(portFile))
            val json = Files.readString(portFile)
            assertTrue(json.contains("\"port\":${server.port}"))
            assertTrue(json.contains("\"project_root\":\"/some/project\""))

            assertEquals(200, get(server.port, server.tokenForTest).statusCode())
            assertEquals(401, get(server.port, null).statusCode())
            assertEquals(401, get(server.port, "wrong").statusCode())
        } finally {
            server.dispose()
        }
        assertFalse(Files.exists(LeanCtxPaths.portFile(dataDir, "/some/project")))
    }

    fun testStartReapsStaleForeignPortFile() {
        val dataDir = Files.createTempDirectory("lc-srv2")
        // Seed a foreign stale file (dead pid) that must be reaped on boot.
        val stale = dataDir.resolve("jetbrains-stale.port")
        PortFileWriter.write(stale, PortFileData(1, "t", Long.MAX_VALUE, "/other", "v", 1L))
        val server = BackendHttpServer(
            dataDir = dataDir, project = project, projectRoot = "/some/project",
            ideVersion = "IC-2026.1.3", projectName = "demo", startedAt = 1L
        )
        try {
            server.start()
            assertFalse("stale foreign port file reaped on boot", Files.exists(stale))
            assertTrue(Files.exists(LeanCtxPaths.portFile(dataDir, "/some/project")))
        } finally {
            server.dispose()
        }
        // dispose() must stop watcher + heartbeat and remove our file (no leak).
        assertFalse(Files.exists(LeanCtxPaths.portFile(dataDir, "/some/project")))
    }

    fun testWatcherReWritesDeletedPortFile() {
        val dataDir = Files.createTempDirectory("lc-srv3")
        val server = BackendHttpServer(
            dataDir = dataDir, project = project, projectRoot = "/some/project",
            ideVersion = "IC-2026.1.3", projectName = "demo", startedAt = 1L
        )
        try {
            server.start()
            val pf = LeanCtxPaths.portFile(dataDir, "/some/project")
            assertTrue(Files.exists(pf))
            Files.delete(pf)
            // The watcher must re-create it.
            var restored = false
            val deadline = System.currentTimeMillis() + 10_000
            while (System.currentTimeMillis() < deadline) {
                if (Files.exists(pf)) {
                    restored = true
                    break
                }
                Thread.sleep(100)
            }
            assertTrue("watcher re-wrote the deleted port file", restored)
        } finally {
            server.dispose()
        }
    }
}
