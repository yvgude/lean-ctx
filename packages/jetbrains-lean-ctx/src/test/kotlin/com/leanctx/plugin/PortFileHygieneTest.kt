package com.leanctx.plugin

import com.intellij.testFramework.fixtures.BasePlatformTestCase
import com.leanctx.plugin.server.BackendHttpServer
import com.leanctx.plugin.server.LeanCtxPaths
import java.nio.file.Files
import java.nio.file.Path

/**
 * Guards that a test run does not leak `jetbrains-*.port` files into the real
 * data dir: we redirect LEAN_CTX_DATA_DIR via a system property (test-injectable
 * override added to LeanCtxPaths.dataDir()), spin up a real BackendHttpServer
 * using that resolved dir, and assert no port file escapes to ~/.lean-ctx.
 */
class PortFileHygieneTest : BasePlatformTestCase() {

    private lateinit var tempDataDir: Path
    private var prevProp: String? = null

    override fun setUp() {
        tempDataDir = Files.createTempDirectory("leanctx-test-datadir")
        prevProp = System.getProperty("LEAN_CTX_DATA_DIR")
        System.setProperty("LEAN_CTX_DATA_DIR", tempDataDir.toString())
        super.setUp()
    }

    override fun tearDown() {
        try {
            super.tearDown()
        } finally {
            if (prevProp != null) {
                System.setProperty("LEAN_CTX_DATA_DIR", prevProp!!)
            } else {
                System.clearProperty("LEAN_CTX_DATA_DIR")
            }
        }
    }

    fun testNoPortFileLeftInRealDataDir() {
        // Verify the system-property override is wired: dataDir() must return tempDataDir.
        val resolved = LeanCtxPaths.dataDir()
        assertEquals(
            "LEAN_CTX_DATA_DIR system property must redirect dataDir() to temp dir",
            tempDataDir, resolved
        )

        // Spin up a real server through the production dataDir() path.
        val server = BackendHttpServer(
            dataDir = LeanCtxPaths.dataDir(),
            project = project,
            projectRoot = "/hygiene-test/project",
            ideVersion = "IC-test",
            projectName = "hygieneTest",
            startedAt = System.currentTimeMillis(),
        )
        try {
            server.start()
            val portFile = LeanCtxPaths.portFile(tempDataDir, "/hygiene-test/project")
            assertTrue("server must write port file into temp data dir", Files.exists(portFile))

            // Assert no port file leaked into the real ~/.lean-ctx.
            val realHome = System.getProperty("user.home")
            val leanCtxDir = Path.of(realHome, ".lean-ctx")
            if (Files.isDirectory(leanCtxDir)) {
                val leaked = Files.list(leanCtxDir).use { stream ->
                    stream.filter { f ->
                        val name = f.fileName.toString()
                        name.startsWith("jetbrains-") && name.endsWith(".port") &&
                            // Only flag a file whose content references our test project root.
                            runCatching {
                                Files.readString(f).contains("hygiene-test")
                            }.getOrDefault(false)
                    }.findAny().isPresent
                }
                assertFalse("test run must not leak a port file into ~/.lean-ctx", leaked)
            }
        } finally {
            server.dispose()
        }

        // After dispose(), the temp data dir must contain no leftover port files.
        val portFile = LeanCtxPaths.portFile(tempDataDir, "/hygiene-test/project")
        assertFalse("dispose() must remove port file from temp data dir", Files.exists(portFile))
    }
}
