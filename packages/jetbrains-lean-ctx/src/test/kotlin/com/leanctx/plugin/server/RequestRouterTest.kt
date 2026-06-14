package com.leanctx.plugin.server

import com.intellij.testFramework.fixtures.BasePlatformTestCase

class RequestRouterTest : BasePlatformTestCase() {

    private fun router() = RequestRouter(
        token = "secret",
        ideVersion = "IC-2026.1.3",
        projectName = "demo",
        project = project,
    )

    fun testHealthWithValidTokenReturns200() {
        val r = router().route("GET", "/health", "secret", "")
        assertEquals(200, r.status)
        assertTrue(r.body.contains("\"status\":\"ok\""))
        assertTrue(r.body.contains("\"ideVersion\":\"IC-2026.1.3\""))
        assertTrue(r.body.contains("\"project\":\"demo\""))
    }

    fun testMissingTokenReturns401() {
        val r = router().route("GET", "/health", null, "")
        assertEquals(401, r.status)
        assertTrue(r.body.contains("UNAUTHORIZED"))
    }

    fun testWrongTokenReturns401() {
        assertEquals(401, router().route("GET", "/health", "nope", "").status)
    }

    fun testUnknownPathWithValidTokenReturns404() {
        val r = router().route("GET", "/nope", "secret", "")
        assertEquals(404, r.status)
    }
}
