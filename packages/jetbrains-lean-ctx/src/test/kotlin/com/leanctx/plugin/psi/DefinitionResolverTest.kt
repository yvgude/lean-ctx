package com.leanctx.plugin.psi

import com.intellij.testFramework.fixtures.BasePlatformTestCase

class DefinitionResolverTest : BasePlatformTestCase() {

    fun testResolvesUsageToDeclaration() {
        val file = myFixture.configureByText(
            "A.kt",
            """
            fun target() {}
            fun caller() { target() }
            """.trimIndent(),
        )
        val locator = PsiLocator(project)
        val resolver = DefinitionResolver(locator)
        // 0-based caret on the "target" call inside caller(): line 1.
        val callLine = 1
        val callCol = file.text.lines()[1].indexOf("target")
        val locs = locator.inSmartReadAction {
            resolver.resolve(file, callLine, callCol)
        }
        assertEquals(1, locs.size)
        // The declaration `fun target()` is on line 0.
        assertEquals(0, locs[0].range.start.line)
    }

    fun testNoSymbolThrows() {
        val file = myFixture.configureByText("A.kt", "fun f() { }\n")
        val locator = PsiLocator(project)
        val resolver = DefinitionResolver(locator)
        val e = org.junit.Assert.assertThrows(com.leanctx.plugin.server.BackendException::class.java) {
            locator.inSmartReadAction {
                resolver.resolve(file, line = 0, character = 8) // inside the empty braces
            }
        }
        assertEquals("NO_SYMBOL_AT_POSITION", e.code)
    }
}
