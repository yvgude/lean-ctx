package com.leanctx.plugin.server

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import com.leanctx.plugin.endpoint.StructureHandlers
import com.leanctx.plugin.spi.StructureProvider
import java.nio.file.Files
import java.nio.file.Paths

class RequestRouterStructureTest : BasePlatformTestCase() {

    // In production the StructureProvider implementation is contributed by the optional
    // leanctx-jvm.xml descriptor (loaded only when org.jetbrains.kotlin is present). The
    // IntelliJ light-test fixture (BasePlatformTestCase) does not merge optional
    // <depends config-file="..."> descriptors, so the project-level EP would be empty here.
    // Register the same StructureHandlers impl into the same EP for the test lifetime to
    // exercise the real RequestRouter -> StructureProvider.forProject(project) wiring.
    override fun setUp() {
        super.setUp()
        StructureProvider.EP.getPoint(project)
            .registerExtension(StructureHandlers(project), testRootDisposable)
    }

    private fun router() = RequestRouter("tok", "IC-2026.1", project.name, project)

    private fun writeSource(name: String, text: String): String {
        val base = project.basePath!!
        Files.createDirectories(Paths.get(base))
        val p = Paths.get(base, name)
        Files.writeString(p, text)
        WriteAction.computeAndWait<Unit, RuntimeException> {
            LocalFileSystem.getInstance().refreshAndFindFileByPath(p.toString())
        }
        return name
    }

    private fun routeOffEdt(method: String, path: String, body: String): HttpResult =
        ApplicationManager.getApplication().executeOnPooledThread<HttpResult> {
            router().route(method, path, "tok", body)
        }.get()

    // End-to-end wiring of /type_hierarchy: parse HierarchyRequest -> StructureHandlers.typeHierarchy
    // (off the EDT) -> wire response. We assert the resolved root node + the truncated flag, NOT the
    // children: the router resolves the file via PsiLocator.psiFile -> LocalFileSystem (project.basePath),
    // which is NOT a registered indexed source root in a light fixture, so neither ClassInheritorsSearch
    // (subtypes) nor light-class supertype resolution populates children here. Tree-building in both
    // directions is covered by TypeHierarchyResolverTest (configureByText fixture, PsiFile passed directly).
    fun testTypeHierarchyRoute() {
        // Unique filename: other test classes (e.g. RequestRouterNavTest) also write A.kt to the same
        // shared project.basePath; a distinct name avoids stale-VFS content bleeding across the full suite.
        val rel = writeSource("HierA.kt", "interface Animal\nclass Dog : Animal\nclass Cat : Animal\n")
        val col = 6 // 0-based char of "Dog" in "class Dog : Animal"
        val body = """{"path":"$rel","line":1,"character":$col,"direction":"supertypes"}"""
        val res = routeOffEdt("POST", "/type_hierarchy", body)
        assertEquals("body=${res.body}", 200, res.status)
        assertTrue("body=${res.body}", res.body.contains("\"tree\""))
        assertTrue("body=${res.body}", res.body.contains("Dog"))
        assertTrue("body=${res.body}", res.body.contains("\"truncated\""))
    }

    fun testSymbolsOverviewRoute() {
        val rel = writeSource("OverB.kt", "interface Animal\nfun main() {}\n")
        val res = routeOffEdt("POST", "/symbols_overview", """{"path":"$rel"}""")
        assertEquals("body=${res.body}", 200, res.status)
        assertTrue("body=${res.body}", res.body.contains("\"symbols\""))
        assertTrue("body=${res.body}", res.body.contains("interface"))
        assertTrue("body=${res.body}", res.body.contains("\"total\""))
    }

    fun testTypeHierarchyWrongTokenIs401() {
        val res = router().route("POST", "/type_hierarchy", "WRONG", "{}")
        assertEquals(401, res.status)
        assertTrue(res.body.contains("UNAUTHORIZED"))
    }

    fun testSymbolsOverviewFileNotFoundIs200Envelope() {
        val res = routeOffEdt("POST", "/symbols_overview", """{"path":"Nope.kt"}""")
        assertEquals(200, res.status)
        assertTrue(res.body.contains("FILE_NOT_FOUND"))
    }

    fun testTypeHierarchyDegradesWhenNoProvider() {
        val router = RequestRouter("tok", "RR-2026.1", project.name, project, structureProvider = null)
        val body = """{"path":"x.rs","line":0,"character":0,"direction":"supertypes"}"""
        val res = router.route("POST", "/type_hierarchy", "tok", body)
        assertEquals("body=${res.body}", 200, res.status)
        assertTrue("body=${res.body}", res.body.contains("UNSUPPORTED_LANGUAGE"))
    }

    fun testSymbolsOverviewDegradesWhenNoProvider() {
        val router = RequestRouter("tok", "RR-2026.1", project.name, project, structureProvider = null)
        val res = router.route("POST", "/symbols_overview", "tok", """{"path":"x.rs"}""")
        assertEquals("body=${res.body}", 200, res.status)
        assertTrue("body=${res.body}", res.body.contains("UNSUPPORTED_LANGUAGE"))
    }
}
