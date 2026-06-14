package com.leanctx.plugin.server

import com.intellij.openapi.Disposable
import com.sun.net.httpserver.HttpServer
import java.net.InetSocketAddress
import java.nio.charset.StandardCharsets
import java.nio.file.Path
import java.security.SecureRandom
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * Per-project localhost HTTP server. lean-ctx (Rust) is the client; this is the server.
 * Disposable → registered against the Project, so projectClosing stops it + deletes the port file.
 */
class BackendHttpServer(
    private val dataDir: Path,
    private val project: com.intellij.openapi.project.Project,
    private val projectRoot: String,
    private val ideVersion: String,
    private val projectName: String,
    private val startedAt: Long,
) : Disposable {
    private val token: String = newToken()
    private var server: HttpServer? = null
    private var executor: ExecutorService? = null
    @Volatile
    private var portFile: Path? = null
    @Volatile
    private var portFileData: PortFileData? = null
    private var watcher: PortFileWatcher? = null
    private var heartbeat: PortFileHeartbeat? = null

    @Volatile
    private var disposed = false

    val port: Int get() = server?.address?.port ?: -1
    val tokenForTest: String get() = token

    fun start() {
        check(server == null) { "BackendHttpServer already started" }
        val http = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        val router = RequestRouter(token, ideVersion, projectName, project)
        val exec = Executors.newCachedThreadPool()
        http.executor = exec
        executor = exec
        http.createContext("/") { exchange ->
            try {
                val headerToken = exchange.requestHeaders.getFirst("X-LeanCtx-Token")
                val body = exchange.requestBody.readBytes().toString(StandardCharsets.UTF_8)
                val result = router.route(exchange.requestMethod, exchange.requestURI.path, headerToken, body)
                val bytes = result.body.toByteArray(StandardCharsets.UTF_8)
                exchange.responseHeaders.add("Content-Type", "application/json")
                exchange.sendResponseHeaders(result.status, bytes.size.toLong())
                exchange.responseBody.use { it.write(bytes) }
            } finally {
                exchange.close()
            }
        }
        http.start()
        server = http

        val pf = LeanCtxPaths.portFile(dataDir, projectRoot)
        // 2. Stale-cleanup at boot, before writing our own file.
        val reaper = StalePortFileReaper(dataDir, pf)
        reaper.reap()

        // 3. Write our own port file. Re-writes reuse this exact identity.
        val data = PortFileData(
            port = http.address.port,
            token = token,
            pid = ProcessHandle.current().pid(),
            projectRoot = projectRoot,
            ideVersion = ideVersion,
            startedAt = startedAt,
        )
        PortFileWriter.write(pf, data)
        portFile = pf
        portFileData = data

        // 4. Watcher: immediate re-write if our file is deleted at runtime.
        watcher = PortFileWatcher(dataDir, pf, ::reWritePortFile)

        // 5. Heartbeat: periodic reap + self-heal fallback (30s).
        heartbeat = PortFileHeartbeat(reaper, pf, ::reWritePortFile).also { it.start() }
    }

    /** Re-write our port file with the stable identity (socket lives on). Atomic + idempotent. */
    private fun reWritePortFile() {
        if (disposed) return
        val pf = portFile ?: return
        val data = portFileData ?: return
        PortFileWriter.write(pf, data)
    }

    override fun dispose() {
        disposed = true
        heartbeat?.cancel()
        heartbeat = null
        watcher?.close()
        watcher = null
        server?.stop(0)
        server = null
        // HttpServer.stop() does not close a user-supplied executor; reclaim its threads now.
        executor?.shutdownNow()
        executor = null
        portFile?.let { PortFileWriter.delete(it) }
        portFile = null
        portFileData = null
    }

    private fun newToken(): String {
        val bytes = ByteArray(32)
        SecureRandom().nextBytes(bytes)
        return buildString(64) { bytes.forEach { append("%02x".format(it)) } }
    }
}
