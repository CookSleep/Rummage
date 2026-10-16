package com.cooksleep.rummage.storage

/** 文档 ID 与真实路径的换算、复制时的文件名；不调用 Android 接口，可在 JVM 上测试 */
object StoragePaths {
  /** 系统自带的外部存储提供者（手机存储与存储卡） */
  const val EXTERNAL = "com.android.externalstorage.documents"

  /** 系统的“下载”提供者，文档 ID 为 `raw:<真实路径>` 或 `msf:<MediaStore 中的 id>` */
  const val DOWNLOADS = "com.android.providers.downloads.documents"

  /** 系统的媒体提供者，文档 ID 为 `image:<id>`、`video:<id>`、`audio:<id>` */
  const val MEDIA = "com.android.providers.media.documents"

  /** 要查询的 MediaStore 表 */
  enum class MediaTable { FILES, IMAGES, VIDEO, AUDIO }

  data class MediaRef(val table: MediaTable, val id: Long)

  /**
   * 下载提供者的 `msf:<id>` 与媒体提供者的 `<类型>:<id>` 对应的 MediaStore 条目，查它的 `DATA` 列得到真实路径；
   * 其他提供者或形式时为空
   */
  fun mediaRef(authority: String?, id: String): MediaRef? {
    val sep = id.indexOf(':')
    if (sep <= 0) return null
    val n = id.substring(sep + 1).toLongOrNull() ?: return null
    val kind = id.substring(0, sep)
    val table = when (authority) {
      DOWNLOADS -> if (kind == "msf") MediaTable.FILES else null
      MEDIA -> when (kind) {
        "image" -> MediaTable.IMAGES
        "video" -> MediaTable.VIDEO
        "audio" -> MediaTable.AUDIO
        else -> null
      }
      else -> null
    } ?: return null
    return MediaRef(table, n)
  }

  /**
   * 外部存储提供者的文档 ID（`卷:相对路径`）换算成真实路径：`primary` 是主存储 [primary]，
   * `home` 是主存储中的 Documents，其他卷按 UUID（不区分大小写）在 [volumes]（UUID → 卷的目录）中找；
   * 不是这种形式或找不到卷时为空
   */
  fun fromDocumentId(id: String, primary: String, volumes: Map<String, String>): String? {
    val sep = id.indexOf(':')
    if (sep <= 0) return null
    val vol = id.substring(0, sep)
    val rel = id.substring(sep + 1).trim('/')
    val root = when (vol) {
      "primary" -> primary
      "home" -> "$primary/Documents"
      else -> volumes.entries.firstOrNull { it.key.equals(vol, ignoreCase = true) }?.value ?: return null
    }
    return if (rel.isEmpty()) root else "$root/$rel"
  }

  /** 不与已有文件重名的文件名：`a.gguf` 已存在时依次试 `a (1).gguf`、`a (2).gguf`…… */
  fun uniqueName(name: String, exists: (String) -> Boolean): String {
    if (!exists(name)) return name
    val dot = name.lastIndexOf('.')
    val base = if (dot > 0) name.substring(0, dot) else name
    val ext = if (dot > 0) name.substring(dot) else ""
    var i = 1
    while (exists("$base ($i)$ext")) i++
    return "$base ($i)$ext"
  }
}
