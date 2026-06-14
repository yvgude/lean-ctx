package com.leanctx.plugin.psi

import com.intellij.testFramework.fixtures.BasePlatformTestCase

class FileStructureScannerTest : BasePlatformTestCase() {

    fun testTopLevelSymbols() {
        val file = myFixture.configureByText(
            "A.kt",
            """
            interface Animal
            class Dog : Animal
            object Registry
            fun freeFun() {}
            val topProp = 1
            """.trimIndent(),
        )
        val scanner = FileStructureScanner(PsiLocator(project))
        val res = locator_scan(scanner, file)
        val byName = res.symbols.associateBy { it.name }
        assertEquals("interface", byName["Animal"]!!.kind)
        assertEquals("class", byName["Dog"]!!.kind)
        assertEquals("object", byName["Registry"]!!.kind)
        assertEquals("function", byName["freeFun"]!!.kind)
        assertEquals("property", byName["topProp"]!!.kind)
        // 1-based lines
        assertEquals(1, byName["Animal"]!!.line)
        assertFalse(res.truncated)
        assertEquals(res.symbols.size, res.total)
    }

    private fun locator_scan(scanner: FileStructureScanner, file: com.intellij.psi.PsiFile) =
        com.intellij.openapi.application.ReadAction.compute<com.leanctx.plugin.dto.SymbolsOverviewResponse, RuntimeException> {
            scanner.scan(file)
        }

    fun testUnsupportedLanguageThrows() {
        val file = myFixture.configureByText("notes.txt", "hello world\n")
        try {
            locator_scan(FileStructureScanner(PsiLocator(project)), file)
            fail("expected UNSUPPORTED_LANGUAGE")
        } catch (e: com.leanctx.plugin.server.BackendException) {
            assertEquals("UNSUPPORTED_LANGUAGE", e.code)
        }
    }
}
