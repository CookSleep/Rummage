package com.cooksleep.rummage.storage

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class StoragePathsTest {
  private val primary = "/storage/emulated/0"
  private val volumes = mapOf("1234-5678" to "/storage/1234-5678")

  @Test
  fun primaryVolume() {
    assertEquals("/storage/emulated/0/DCIM/Camera", StoragePaths.fromDocumentId("primary:DCIM/Camera", primary, volumes))
    assertEquals("/storage/emulated/0", StoragePaths.fromDocumentId("primary:", primary, volumes))
    assertEquals("/storage/emulated/0/Download/书", StoragePaths.fromDocumentId("primary:Download/书/", primary, volumes))
  }

  @Test
  fun homeIsDocuments() {
    assertEquals("/storage/emulated/0/Documents/笔记", StoragePaths.fromDocumentId("home:笔记", primary, volumes))
  }

  @Test
  fun sdCard() {
    assertEquals("/storage/1234-5678/Photos", StoragePaths.fromDocumentId("1234-5678:Photos", primary, volumes))
    // UUID 的大小写可能与卷信息中的不同
    assertEquals("/storage/ABCD-12EF", StoragePaths.fromDocumentId("ABCD-12EF:", primary, mapOf("abcd-12ef" to "/storage/ABCD-12EF")))
  }

  @Test
  fun unknown() {
    assertNull(StoragePaths.fromDocumentId("ABCD-0000:Photos", primary, volumes))
    assertNull(StoragePaths.fromDocumentId("Photos", primary, volumes))
    assertNull(StoragePaths.fromDocumentId(":Photos", primary, volumes))
  }

  @Test
  fun toDocumentId() {
    assertEquals("primary:DCIM/Camera", StoragePaths.toDocumentId("/storage/emulated/0/DCIM/Camera", primary, volumes))
    assertEquals("primary:", StoragePaths.toDocumentId("/storage/emulated/0", primary, volumes))
    assertEquals("1234-5678:Photos/书", StoragePaths.toDocumentId("/storage/1234-5678/Photos/书", primary, volumes))
    // 只是前缀相同的其他目录不算在卷中
    assertNull(StoragePaths.toDocumentId("/storage/emulated/01/a", primary, volumes))
    assertNull(StoragePaths.toDocumentId("/data/data/x", primary, volumes))
  }

  @Test
  fun mediaRef() {
    val d = StoragePaths.DOWNLOADS
    val m = StoragePaths.MEDIA
    assertEquals(StoragePaths.MediaRef(StoragePaths.MediaTable.FILES, 1234), StoragePaths.mediaRef(d, "msf:1234"))
    assertEquals(StoragePaths.MediaRef(StoragePaths.MediaTable.IMAGES, 42), StoragePaths.mediaRef(m, "image:42"))
    assertEquals(StoragePaths.MediaRef(StoragePaths.MediaTable.VIDEO, 7), StoragePaths.mediaRef(m, "video:7"))
    assertEquals(StoragePaths.MediaRef(StoragePaths.MediaTable.AUDIO, 8), StoragePaths.mediaRef(m, "audio:8"))
    // raw: 直接是路径；类型与提供者不对应、id 不是数字、其他提供者都不查
    assertNull(StoragePaths.mediaRef(d, "raw:/storage/emulated/0/Download/a.gguf"))
    assertNull(StoragePaths.mediaRef(d, "image:42"))
    assertNull(StoragePaths.mediaRef(m, "msf:42"))
    assertNull(StoragePaths.mediaRef(m, "document:42"))
    assertNull(StoragePaths.mediaRef(m, "image:abc"))
    assertNull(StoragePaths.mediaRef(StoragePaths.EXTERNAL, "msf:1"))
    assertNull(StoragePaths.mediaRef(null, "msf:1"))
  }

  @Test
  fun uniqueName() {
    val taken = setOf("a.gguf", "a (1).gguf", "noext")
    assertEquals("b.gguf", StoragePaths.uniqueName("b.gguf") { it in taken })
    assertEquals("a (2).gguf", StoragePaths.uniqueName("a.gguf") { it in taken })
    assertEquals("noext (1)", StoragePaths.uniqueName("noext") { it in taken })
  }
}
