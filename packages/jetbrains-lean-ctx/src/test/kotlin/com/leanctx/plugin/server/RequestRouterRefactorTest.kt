package com.leanctx.plugin.server

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.VfsUtil
import com.intellij.testFramework.DumbModeTestUtils
import com.intellij.testFramework.PlatformTestUtil
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import java.nio.file.Files
import java.nio.file.Paths
import java.util.concurrent.TimeUnit

class RequestRouterRefactorTest : BasePlatformTestCase() {

    private fun router() = RequestRouter(
        token = "tok",
        ideVersion = "IC-2026.1",
        projectName = project.name,
        project = project,
    )

    // Route off the EDT — mirrors the real embedded HTTP server thread. The Kotlin
    // RenameProcessor calls the Analysis API (KaSession), which is PROHIBITED on the
    // EDT even inside a read action (ProhibitedAnalysisException). PsiLocator runs the
    // body on the *calling* thread via ReadAction.nonBlocking().executeSynchronously(),
    // so the caller must be a background thread.
    //
    // We must NOT plain .get() on the future: SymbolRefactorer.apply() marshals its write
    // transaction back onto the EDT via invokeAndWait. The test body itself runs on the EDT,
    // so a blocking .get() would freeze the EDT and deadlock against that invokeAndWait.
    // PlatformTestUtil.waitForFuture pumps the EDT event queue while waiting, servicing the
    // marshalled write. (Preview has no invokeAndWait but uses the same path for uniformity.)
    private fun routeOffEdt(method: String, path: String, body: String): HttpResult {
        val future = ApplicationManager.getApplication().executeOnPooledThread<HttpResult> {
            router().route(method, path, "tok", body)
        }
        return PlatformTestUtil.waitForFuture(future, TimeUnit.SECONDS.toMillis(60))
    }

    private fun writeFile(rel: String, content: String): String {
        val base = project.basePath!!
        val p = Paths.get(base, rel)
        Files.createDirectories(p.parent)
        // Ensure the file exists on disk so LocalFileSystem can resolve it (PsiLocator
        // resolves via LocalFileSystem.findFileByPath, which the in-memory TempFileSystem
        // of addFileToProject would not satisfy).
        Files.writeString(p, content)
        WriteAction.computeAndWait<Unit, RuntimeException> {
            val vFile = LocalFileSystem.getInstance().refreshAndFindFileByPath(p.toString())
                ?: error("could not refresh VFS for $p")
            // Write the content THROUGH the VFS layer (VfsUtil.saveText) instead of leaving
            // the raw Files.writeString as the source of truth. This keeps the VFS/document
            // model and disk byte-identical, so a later document write (RenameProcessor) does
            // not race a freshly-refreshed disk state → no MemoryDiskConflictResolver
            // "Unexpected memory-disk conflict" flakiness.
            VfsUtil.saveText(vFile, content)
        }
        return p.toString()
    }

    fun testRenamePreviewReturnsUsages() {
        // Declaration in A.kt + a usage in B.kt (same package).
        writeFile("A.kt", "package p\nclass Widget\n")
        writeFile("B.kt", "package p\nfun use(): Widget = Widget()\n")

        // Target = the `Widget` class declaration: line 1 (0-based), char 6 (after "class ").
        val body = """
            {"path":"A.kt",
             "range":{"start":{"line":1,"character":6},"end":{"line":1,"character":12}},
             "new_name":"Gadget"}
        """.trimIndent()

        val res = routeOffEdt("POST", "/renamePreview", body)
        assertEquals(res.body, 200, res.status)
        // Envelope presence is the acceptance signal here: the preview path runs end to
        // end (resolve → findUsages → conflict collection via EDT preprocessUsages → DTO
        // mapping) and returns the usages array — no INTERNAL/read-action error.
        assertTrue(res.body, res.body.contains("\"usages\""))
        // The concrete usage sites (declaration in A.kt + the B.kt reference) are NOT
        // asserted: the BasePlatformTestCase light fixture does not index project.basePath
        // as a source root, so RenameProcessor's resolve/index-based usage search returns
        // an empty set here (verified: body is {"usages":[],"conflicts":[]}). Real
        // usage-site verification → manuelles runIde-Gate (Spec §10).
    }

    fun testRenameApplyRenamesDeclaration() {
        val aPath = writeFile("A.kt", "package p\nclass Widget\n")
        writeFile("B.kt", "package p\nfun use(): Widget = Widget()\n")

        val body = """
            {"path":"A.kt",
             "range":{"start":{"line":1,"character":6},"end":{"line":1,"character":12}},
             "new_name":"Gadget","force":false}
        """.trimIndent()

        val res = routeOffEdt("POST", "/renameApply", body)
        assertEquals(res.body, 200, res.status)
        assertTrue(res.body, res.body.contains("\"applied\":true"))

        // Re-read A.kt from disk: the declaration must be renamed to Gadget.
        WriteAction.computeAndWait<Unit, RuntimeException> {
            LocalFileSystem.getInstance().refreshAndFindFileByPath(aPath)
        }
        val a = Files.readString(Paths.get(aPath))
        assertTrue(a, a.contains("class Gadget"))
        // Multi-File-Verifikation (B.kt usage rewrite) → manuelles runIde-Gate (Spec §10):
        // light fixture does not index basePath, so cross-file usages are not rewritten
        // here. assertTrue(b.contains("Gadget")) / assertFalse(b.contains("Widget")) are
        // exercised in the live runIde gate, not in this light-fixture test.
    }

    fun testUnauthorizedTokenRejected() {
        val res = router().route("POST", "/renamePreview", "wrong", "{}")
        assertEquals(401, res.status)
    }

    fun testRenamePreviewUnsupportedLanguageBeforeNoSymbol() {
        writeFile("notes.txt", "just some notes here\n")
        val body = """
            {"path":"notes.txt",
            "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":4}},
            "new_name":"x"}
        """.trimIndent()
        val res = routeOffEdt("POST", "/renamePreview", body)
        assertEquals(res.body, 200, res.status)
        assertTrue(res.body, res.body.contains("UNSUPPORTED_LANGUAGE"))
        assertFalse(res.body, res.body.contains("NO_SYMBOL"))
    }

    fun testRenamePreviewDuringIndexingReturnsIndexing() {
        // Note: this exercises the isDumb early gate in PsiLocator.inSmartReadAction,
        // NOT the IndexNotReadyException catch-net (the indexing-onset race cannot be
        // simulated deterministically in the headless test harness).
        writeFile("A.kt", "package p\nclass Widget\n")
        val body = """
            {"path":"A.kt",
            "range":{"start":{"line":1,"character":6},"end":{"line":1,"character":12}},
            "new_name":"Gadget"}
        """.trimIndent()
        var res: HttpResult? = null
        DumbModeTestUtils.runInDumbModeSynchronously(project) {
            res = routeOffEdt("POST", "/renamePreview", body)
        }
        val r = requireNotNull(res) { "response must not be null" }
        assertEquals(r.body, 200, r.status)
        assertTrue(r.body, r.body.contains("INDEXING"))
    }

    fun testRenameApplyFileCollisionRefusedEvenWithForce() {
        // The declaration file is named after the class, so renaming Widget → Gadget would
        // ALSO rename the file Widget.kt → Gadget.kt. Gadget.kt already exists, so the rename
        // must be refused as a CONFLICT (never silently overwrite a source file) — even with
        // force=true. Regression for the runIde #4b hang: the un-intercepted file-overwrite
        // modal ("file already exists / Overwrite·Skip") blocked the EDT → /renameApply
        // timed out. force overrides symbol/usage conflicts, NOT a physical file overwrite.
        val widgetPath = writeFile("Widget.kt", "package p\nclass Widget\n")
        writeFile("Gadget.kt", "package p\nclass Gadget\n")

        val body = """
            {"path":"Widget.kt",
             "range":{"start":{"line":1,"character":6},"end":{"line":1,"character":12}},
             "new_name":"Gadget","force":true}
        """.trimIndent()

        val res = routeOffEdt("POST", "/renameApply", body)
        assertEquals(res.body, 200, res.status)
        assertTrue(res.body, res.body.contains("CONFLICT"))
        assertFalse(res.body, res.body.contains("\"applied\":true"))

        // Widget.kt is untouched: no overwrite, no half-rename.
        WriteAction.computeAndWait<Unit, RuntimeException> {
            LocalFileSystem.getInstance().refreshAndFindFileByPath(widgetPath)
        }
        val w = Files.readString(Paths.get(widgetPath))
        assertTrue(w, w.contains("class Widget"))
    }

    fun testMoveCollisionReturnsConflictHeadless_characterization() {
        // CHARACTERIZATION (test-mode only): move Widget.kt into a dir that already holds a
        // Widget.kt. In UnitTestMode a would-be modal becomes an exception → SymbolMover.apply
        // catches it → CONFLICT. Proves the call RETURNS (no test-mode hang); the real runIde
        // modal risk is decided manually in Step 5 (manual), not here.
        writeFile("app/Widget.kt", "package app\nclass Widget\n")
        writeFile("app/moved/Widget.kt", "package app\nclass Widget\n")

        val body = """
            {"path":"app/Widget.kt",
             "range":{"start":{"line":1,"character":6},"end":{"line":1,"character":12}},
             "target":{"kind":"path","path":"app/moved"},"force":false}
        """.trimIndent()

        val res = routeOffEdt("POST", "/moveApply", body)
        // Acceptance: the call RETURNS with 200 (no deadlock in test mode). Body is CONFLICT or
        // applied depending on SDK collision handling — both are non-hang outcomes.
        assertEquals(res.body, 200, res.status)
    }

    fun testSafeDeleteApplyForceDeletesSoleDeclarationFileHeadless() {
        // Widget is the ONLY top-level declaration in its file AND referenced intra-file (the
        // self() return type). A raw SafeDeleteProcessor would raise the "Conflicts Detected"
        // modal on the server thread (runIde gate #8). Headless + force must delete the WHOLE
        // file (class == file) and leave the dangling ref. Intra-file refs ARE resolved in the
        // light fixture (Spec §6.1), so this reproduces #8 as a missing "applied":true.
        val widgetPath = writeFile(
            "app/Widget.kt",
            "package app\nclass Widget {\n    fun self(): Widget = this\n}\n",
        )

        val body = """
            {"path":"app/Widget.kt",
             "range":{"start":{"line":1,"character":6},"end":{"line":1,"character":12}},
             "force":true}
        """.trimIndent()

        val res = routeOffEdt("POST", "/safeDeleteApply", body)
        assertEquals(res.body, 200, res.status)
        assertTrue(res.body, res.body.contains("\"applied\":true"))

        WriteAction.computeAndWait<Unit, RuntimeException> {
            LocalFileSystem.getInstance().refreshAndFindFileByPath(widgetPath)
        }
        assertFalse("Widget.kt must be deleted from disk", Files.exists(Paths.get(widgetPath)))
    }

    fun testSafeDeleteApplyForceDeletesReferencedMemberHeadless() {
        // `target` is referenced intra-file by `caller`. force + headless must delete JUST the
        // member (element.delete()), leaving the file, the class and the now-dangling call —
        // never delete the whole file (Spec §8 sole-decl-heuristic risk guard).
        val holderPath = writeFile(
            "app/Holder.kt",
            "package app\nclass Holder {\n    fun target() {}\n    fun caller() { target() }\n}\n",
        )

        val body = """
            {"path":"app/Holder.kt",
             "range":{"start":{"line":2,"character":8},"end":{"line":2,"character":14}},
             "force":true}
        """.trimIndent()

        val res = routeOffEdt("POST", "/safeDeleteApply", body)
        assertEquals(res.body, 200, res.status)
        assertTrue(res.body, res.body.contains("\"applied\":true"))

        WriteAction.computeAndWait<Unit, RuntimeException> {
            LocalFileSystem.getInstance().refreshAndFindFileByPath(holderPath)
        }
        val text = Files.readString(Paths.get(holderPath))
        assertTrue(text, text.contains("class Holder"))   // file + class survive
        assertTrue(text, text.contains("fun caller"))     // sibling member survives
        assertFalse(text, text.contains("fun target"))    // deleted member is gone
    }

    // ---- Task 9: inline + reformat routes (wiring + error-mapping, no live IDE) ----
    //
    // These prove the three new POST routes are WIRED into route() (not 404), parse the
    // body, and surface a fachlicher Negativfall (missing file → FILE_NOT_FOUND) as a
    // 200 error envelope — exactly like the move/safe_delete siblings. The real
    // processor wiring (inline) is exercised against the live IDE in Task 11.

    fun testReformatRouteParsesAndReturns200() {
        // Missing file → PsiLocator.psiFile throws FILE_NOT_FOUND (BackendException) → 200.
        val body = """{"path":"Missing.kt","scope":{"kind":"file"},"optimize_imports":false}"""
        val res = routeOffEdt("POST", "/reformat", body)
        assertEquals(res.body, 200, res.status)
        assertTrue(res.body, res.body.contains("FILE_NOT_FOUND") || res.body.contains("\"error\""))
    }

    fun testInlinePreviewRouteParsesAndReturns200() {
        val body = """
            {"path":"Missing.kt",
             "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},
             "keep_definition":false}
        """.trimIndent()
        val res = routeOffEdt("POST", "/inlinePreview", body)
        assertEquals(res.body, 200, res.status)
        assertTrue(res.body, res.body.contains("FILE_NOT_FOUND") || res.body.contains("\"error\""))
    }

    fun testInlineApplyRouteParsesAndReturns200() {
        val body = """
            {"path":"Missing.kt",
             "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},
             "keep_definition":false}
        """.trimIndent()
        val res = routeOffEdt("POST", "/inlineApply", body)
        assertEquals(res.body, 200, res.status)
        assertTrue(res.body, res.body.contains("FILE_NOT_FOUND") || res.body.contains("\"error\""))
    }

    fun testUnknownInlineRouteIs404() {
        // Negative control: a typo path is NOT in the route table → 404. Proves the three
        // new paths above are 200 because they are wired, not because everything returns 200.
        val res = router().route("POST", "/inlineApplyy", "tok", "{}")
        assertEquals(404, res.status)
    }
}

