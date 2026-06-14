package com.leanctx.plugin.psi

import com.intellij.testFramework.fixtures.BasePlatformTestCase

class PsiLocatorTest : BasePlatformTestCase() {

    fun testOffsetFromZeroBasedLineChar() {
        val file = myFixture.configureByText("A.kt", "class A\nfun f() {}\n")
        val locator = PsiLocator(project)
        // 0-based: line 1, character 4 -> offset of 'f' in "fun f"
        val offset = locator.offsetOf(file, line = 1, character = 4)
        assertEquals("class A\n".length + 4, offset)
    }

    fun testOutOfRangeLineThrowsPositionError() {
        val file = myFixture.configureByText("A.kt", "class A\n")
        val locator = PsiLocator(project)
        val e = org.junit.Assert.assertThrows(com.leanctx.plugin.server.BackendException::class.java) {
            locator.offsetOf(file, line = 99, character = 0)
        }
        assertEquals("POSITION_OUT_OF_RANGE", e.code)
    }

    fun testToLocationRelativePathAndZeroBasedRange() {
        val file = myFixture.configureByText("A.kt", "class Foo\n")
        val locator = PsiLocator(project)
        val psiClass = file.firstChild // KtClass-ish; use the file's first named element's range
        val loc = locator.toLocation(psiClass)
        assertNotNull(loc)
        assertEquals(0, loc!!.range.start.line) // first line, 0-based
    }
}
