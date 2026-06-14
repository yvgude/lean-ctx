package com.leanctx.plugin.psi

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.VfsUtil
import com.intellij.testFramework.IndexingTestUtil
import com.intellij.testFramework.LightProjectDescriptor
import com.intellij.testFramework.PlatformTestUtil
import com.intellij.testFramework.PsiTestUtil
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import com.leanctx.plugin.dto.PositionDTO
import com.leanctx.plugin.dto.RenamePreviewRequest
import com.leanctx.plugin.dto.TextRangeDTO
import java.nio.file.Files
import java.nio.file.Paths
import java.util.concurrent.TimeUnit

class SymbolRefactorerTest : BasePlatformTestCase() {

    // Class-private light project (fresh descriptor instance, not the shared default) so the
    // PsiTestUtil.addSourceRoot mutation in writeAndIndex — and the asynchronous dumb-mode
    // reindex it triggers — cannot leak into later-running classes (e.g. RequestRouter*Test),
    // which would otherwise fail fast with INDEXING in PsiLocator. The isolated project is
    // disposed at teardown, so its dumb/index state never reaches another class's project.
    private val isolatedDescriptor = LightProjectDescriptor()

    override fun getProjectDescriptor(): LightProjectDescriptor = isolatedDescriptor

    private val fixture = """
        package p

        class Outer {
            fun target() {}
        }

        val shared = Outer()
        fun a() { shared.target() }
        fun b() { shared.target() }
    """.trimIndent()

    // Resolve + ReferencesSearch touch the Kotlin Analysis API (KaSession), prohibited on
    // the EDT. The test body runs on the EDT, so run preview on a pooled thread and pump
    // the EDT while waiting (mirrors RequestRouterRefactorTest.routeOffEdt).
    private fun <T> offEdt(block: () -> T): T {
        val future = ApplicationManager.getApplication().executeOnPooledThread<T> { block() }
        return PlatformTestUtil.waitForFuture(future, TimeUnit.SECONDS.toMillis(60))
    }

    // Write the file to disk (so LocalFileSystem.findFileByPath succeeds in PsiLocator) and
    // register its parent dir as a source root (so ReferencesSearch scope includes it). The
    // module belongs to this class's isolated project, so the root mutation is not cleaned up
    // (the project is disposed at teardown). We still settle indexing so the resolve below runs
    // in smart mode.
    private fun writeAndIndex(rel: String, content: String) {
        val p = Paths.get(project.basePath!!, rel)
        Files.createDirectories(p.parent)
        Files.writeString(p, content)
        WriteAction.computeAndWait<Unit, RuntimeException> {
            val vFile = LocalFileSystem.getInstance().refreshAndFindFileByPath(p.toString())
                ?: error("could not refresh VFS for $p")
            VfsUtil.saveText(vFile, content)
            val module = ModuleManager.getInstance(project).modules.first()
            PsiTestUtil.addSourceRoot(module, vFile.parent)
        }
        IndexingTestUtil.waitUntilIndexesAreReady(project)
    }

    fun testResolvesIndentedMemberNotEnclosingClass() {
        writeAndIndex("Sample.kt", fixture)
        val req = RenamePreviewRequest(
            path = "Sample.kt",
            range = TextRangeDTO(PositionDTO(3, 0), PositionDTO(3, 0)),
            new_name = "renamed",
        )
        val resp = offEdt { SymbolRefactorer(project).preview(req) }
        // Renaming target() finds its two call sites; renaming the enclosing Outer would
        // instead surface the `Outer()` usage (context "val shared = Outer()").
        assertEquals(2, resp.usages.count { it.context?.contains("shared.target()") == true })
    }
}
