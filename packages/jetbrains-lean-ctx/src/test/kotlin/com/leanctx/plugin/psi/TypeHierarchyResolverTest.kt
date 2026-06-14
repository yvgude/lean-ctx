package com.leanctx.plugin.psi

import com.intellij.openapi.application.ApplicationManager
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import com.leanctx.plugin.dto.TypeHierarchyResponse

class TypeHierarchyResolverTest : BasePlatformTestCase() {

    private fun resolve(
        file: com.intellij.psi.PsiFile, line: Int, character: Int, direction: String, scope: String = "project",
    ): TypeHierarchyResponse {
        val locator = PsiLocator(project)
        val resolver = TypeHierarchyResolver(locator)
        // K2 inheritor/supertype resolution uses the Analysis API → forbidden on EDT.
        return ApplicationManager.getApplication().executeOnPooledThread<TypeHierarchyResponse> {
            locator.inSmartReadAction { resolver.resolve(file, line, character, direction, scope) }
        }.get()
    }

    fun testSubtypesOfInterface() {
        val file = myFixture.configureByText(
            "A.kt",
            """
            interface Animal
            class Dog : Animal
            class Cat : Animal
            """.trimIndent(),
        )
        val col = file.text.lines()[0].indexOf("Animal")
        val res = resolve(file, line = 0, character = col, direction = "subtypes")
        assertEquals("Animal", res.tree.name)
        val childNames = res.tree.children.map { it.name }.toSet()
        assertEquals(setOf("Dog", "Cat"), childNames)
        assertFalse(res.truncated)
    }

    fun testSupertypesOfClass() {
        val file = myFixture.configureByText(
            "B.kt",
            """
            interface Animal
            open class Pet : Animal
            class Dog : Pet()
            """.trimIndent(),
        )
        val dogLine = 2
        val col = file.text.lines()[dogLine].indexOf("Dog")
        val res = resolve(file, line = dogLine, character = col, direction = "supertypes")
        assertEquals("Dog", res.tree.name)
        val superNames = res.tree.children.map { it.name }.toSet()
        assertTrue("supers=$superNames", superNames.contains("Pet"))
    }

    fun testNoSymbolAtPosition() {
        val file = myFixture.configureByText("C.kt", "class X\n")
        val locator = PsiLocator(project)
        val resolver = TypeHierarchyResolver(locator)
        // The no-symbol path throws BEFORE any K2 inheritor/supertype search (resolveNamed fails on
        // offset resolution), so it is EDT-safe and needs no pooled thread. Running it off-EDT would
        // make the platform LOG.error the escaped BackendException → the test logger fails the test.
        // Mirror DefinitionResolverTest.testNoSymbolThrows: assertThrows directly inside inSmartReadAction.
        // 0:0 lands on the `class` keyword, not the name identifier → NO_SYMBOL_AT_POSITION.
        val e = org.junit.Assert.assertThrows(com.leanctx.plugin.server.BackendException::class.java) {
            locator.inSmartReadAction {
                resolver.resolve(file, line = 0, character = 0, direction = "supertypes", scope = "project")
            }
        }
        assertEquals("NO_SYMBOL_AT_POSITION", e.code)
    }
}
