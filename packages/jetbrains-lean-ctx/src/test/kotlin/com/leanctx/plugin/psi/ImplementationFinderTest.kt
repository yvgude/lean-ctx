package com.leanctx.plugin.psi

import com.intellij.openapi.application.ApplicationManager
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import com.leanctx.plugin.dto.LocationsResponse

class ImplementationFinderTest : BasePlatformTestCase() {

    fun testFindsInterfaceImplementations() {
        val file = myFixture.configureByText(
            "A.kt",
            """
            interface Animal
            class Dog : Animal
            class Cat : Animal
            """.trimIndent(),
        )
        val locator = PsiLocator(project)
        val finder = ImplementationFinder(locator)
        val ifaceCol = file.text.lines()[0].indexOf("Animal")
        // Kotlin K2 inheritor search uses the Analysis API, which is forbidden on the EDT.
        // Production runs the finder on the background HTTP-handler thread, so mirror that here.
        val result = ApplicationManager.getApplication().executeOnPooledThread<LocationsResponse> {
            locator.inSmartReadAction {
                finder.find(file, line = 0, character = ifaceCol, scope = "project")
            }
        }.get()
        // Dog + Cat
        assertEquals(2, result.locations.size)
        assertFalse(result.truncated)
    }
}
