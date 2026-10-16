package com.cooksleep.rummage.storage

import org.junit.Assert.assertEquals
import org.junit.Test

class SafeAreaTest {
  @Test
  fun convertsDevicePixels() {
    assertEquals(Insets(32.0, 0.0, 16.0, 0.0), SafeArea.fromPx(96, 0, 48, 0, 3f))
    assertEquals(Insets(24.73, 0.0, 0.0, 0.0), SafeArea.fromPx(68, 0, 0, 0, 2.75f))
  }

  @Test
  fun scriptSetsAllSides() {
    assertEquals(
      "(function(s){s.setProperty('--safe-area-inset-top','32.0px');s.setProperty('--safe-area-inset-right','0.0px');" +
        "s.setProperty('--safe-area-inset-bottom','16.0px');s.setProperty('--safe-area-inset-left','0.0px');})(document.documentElement.style)",
      SafeArea.script(Insets(32.0, 0.0, 16.0, 0.0)),
    )
  }
}
