package com.leanctx.plugin.server

import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import java.nio.file.Files
import java.nio.file.Paths

class RequestRouterEditTest : BasePlatformTestCase() {

    private fun router() = RequestRouter(
        token = "tok",
        ideVersion = "IC-2026.1",
        projectName = project.name,
        project = project,
    )

    fun testReplaceSymbolBodyWritesRange() {
        // Same on-disk fixture strategy as RequestRouterNavTest: PsiLocator resolves via
        // LocalFileSystem, which cannot see the in-memory TempFileSystem of configureByText.
        // We write the source into the real project.basePath and use the project-relative path.
        val base = project.basePath!!
        Files.createDirectories(Paths.get(base))
        val kt = Paths.get(base, "Foo.kt")
        Files.writeString(kt, "class A {\n    fun b() { 1 }\n}\n")
        WriteAction.computeAndWait<Unit, RuntimeException> {
            LocalFileSystem.getInstance().refreshAndFindFileByPath(kt.toString())
        }

        // Line 1 = "    fun b() { 1 }" (length 17); replace chars 0..17 with the new body.
        val body = """
            {"path":"Foo.kt",
             "range":{"start":{"line":1,"character":0},"end":{"line":1,"character":17}},
             "text":"    fun b() { 2 }"}
        """.trimIndent()

        val res = router().route("POST", "/replaceSymbolBody", "tok", body)
        assertEquals(res.body, 200, res.status)
        assertTrue(res.body, res.body.contains("\"applied\":true"))

        val after = Files.readString(kt)
        assertTrue(after, after.contains("fun b() { 2 }"))
    }
}
