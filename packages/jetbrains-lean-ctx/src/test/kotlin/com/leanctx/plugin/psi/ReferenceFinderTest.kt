package com.leanctx.plugin.psi

import com.intellij.testFramework.fixtures.BasePlatformTestCase

class ReferenceFinderTest : BasePlatformTestCase() {

    fun testFindsAllUsagesInProjectScope() {
        val file = myFixture.configureByText(
            "A.kt",
            """
            fun target() {}
            fun a() { target() }
            fun b() { target() }
            """.trimIndent(),
        )
        val locator = PsiLocator(project)
        val finder = ReferenceFinder(locator)
        val declCol = file.text.lines()[0].indexOf("target")
        val result = locator.inSmartReadAction {
            finder.find(file, line = 0, character = declCol, scope = "project")
        }
        // two call sites
        assertEquals(2, result.locations.size)
        assertFalse(result.truncated)
        assertEquals(2, result.total)
    }

    fun testResolvesIndentedMemberNotEnclosingClass() {
        val file = myFixture.configureByText(
            "Sample.kt",
            """
            class Outer {
                fun target() {}
            }
            val shared = Outer()
            fun a() { shared.target() }
            fun b() { shared.target() }
            """.trimIndent(),
        )
        val locator = PsiLocator(project)
        val finder = ReferenceFinder(locator)
        // target() is on line 1 (0-based), indented; address char 0 (lands on indentation).
        val result = locator.inSmartReadAction {
            finder.find(file, line = 1, character = 0, scope = "project")
        }
        // Correct resolution → the two `shared.target()` call sites (lines 4 and 5).
        // Wrong resolution (enclosing class Outer) → its single `Outer()` usage (line 3).
        assertEquals(2, result.locations.size)
        assertEquals(setOf(4, 5), result.locations.map { it.range.start.line }.toSet())
    }
}
