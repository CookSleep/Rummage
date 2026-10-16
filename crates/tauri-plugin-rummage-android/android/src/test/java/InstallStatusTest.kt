package com.cooksleep.rummage.storage

import org.junit.Assert.assertEquals
import org.junit.Test

class InstallStatusTest {
  @Test
  fun kinds() {
    // PackageInstaller 的取值：-1 等用户确认，0 成功，3 用户取消
    assertEquals("pending", InstallStatus.kind(-1))
    assertEquals("success", InstallStatus.kind(0))
    assertEquals("aborted", InstallStatus.kind(3))
    for (code in listOf(1, 2, 4, 5, 6, 7, 8, 99)) assertEquals("failure", InstallStatus.kind(code))
  }

  @Test
  fun names() {
    assertEquals("STATUS_FAILURE_CONFLICT", InstallStatus.name(5))
    assertEquals("STATUS_FAILURE_INCOMPATIBLE", InstallStatus.name(7))
    assertEquals("STATUS_99", InstallStatus.name(99))
  }
}
