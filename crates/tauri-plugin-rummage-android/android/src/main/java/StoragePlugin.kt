package com.cooksleep.rummage.storage

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ContentUris
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.PowerManager
import android.os.storage.StorageManager
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.provider.Settings
import android.view.View
import android.webkit.MimeTypeMap
import android.webkit.WebView
import androidx.activity.result.ActivityResult
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.FileProvider
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import app.tauri.Logger
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.IOException

@InvokeArg
class ProgressArgs {
  var done: Long = 0
  var total: Long = 0
  /** 暂停原因；为空时显示进度 */
  var note: String? = null
}

@InvokeArg
class OpenFileArgs {
  lateinit var path: String
}

@InvokeArg
class InstallArgs {
  lateinit var path: String
}

@InvokeArg
class PickFilesArgs {
  lateinit var mimeTypes: Array<String>
  var multiple: Boolean = false
  /** 换算不出可读的真实路径时，复制到这个目录 */
  lateinit var copyTo: String
}

/**
 * 存储权限、文件选择、后台建立索引、安装更新与界面的几项系统功能，只由 Rust 侧调用（crates/tauri-plugin-rummage-android/src/lib.rs）。
 * 选择的文件夹与文件换算成真实路径，读取靠“所有文件访问”权限，不持有文档提供者的 URI 授权。
 * 前台服务见 [IndexService]；应用回到前台（[onResume]）作为 `resume` 事件交给 Rust 侧。
 * 安全区见 [load]：页面加载前算出的值写不进页面，前端启动时再用 `safeArea` 取一次
 */
@TauriPlugin
class StoragePlugin(private val activity: Activity) : Plugin(activity) {
  @Volatile
  private var insets = Insets(0.0, 0.0, 0.0, 0.0)

  /**
   * 全面屏（targetSdk 35 起强制）下页面铺到状态栏、导航栏后面，Android WebView 140 以前 `env(safe-area-inset-*)` 拿不到正确的值：
   * 系统栏与挖孔的宽度换算成 CSS 像素，变化时写进页面的 CSS 变量（[SafeArea]）。
   * 全面屏下 `adjustResize` 也不再让出输入法：给 WebView 的父容器留出输入法的高度，聚焦的输入框才能滚到可见处；
   * 输入法盖住了导航栏，这时页面底部不再让出导航栏。最后交还 WebView 自己处理，新版 WebView 据此给出 `env()` 的值
   */
  override fun load(webView: WebView) {
    ViewCompat.setOnApplyWindowInsetsListener(webView) { v, all ->
      val bars = all.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      val ime = all.getInsets(WindowInsetsCompat.Type.ime()).bottom
      (v.parent as? View)?.let { p -> if (p.paddingBottom != ime) p.setPadding(0, 0, 0, ime) }
      val next = SafeArea.fromPx(bars.top, bars.right, if (ime > 0) 0 else bars.bottom, bars.left, v.resources.displayMetrics.density)
      if (next != insets) {
        insets = next
        webView.evaluateJavascript(SafeArea.script(next), null)
      }
      ViewCompat.onApplyWindowInsets(v, all)
    }
    webView.requestApplyInsets()
  }

  @Command
  fun safeArea(invoke: Invoke) {
    val i = insets
    val ret = JSObject()
    ret.put("top", i.top)
    ret.put("right", i.right)
    ret.put("bottom", i.bottom)
    ret.put("left", i.left)
    invoke.resolve(ret)
  }

  /** 把应用退到后台：返回键在界面上已经没有可关闭的内容时调用，不结束 Activity，后台建立索引继续 */
  @Command
  fun moveToBack(invoke: Invoke) {
    activity.moveTaskToBack(true)
    invoke.resolve()
  }

  /**
   * 用其他应用打开文件：经应用的 FileProvider（gen/android 的 `file_paths.xml`）换成 content URI，按扩展名带上 MIME 类型，
   * 只给这一次打开授予读取权限。没有能打开它的应用时返回 `{ opened: false }`
   */
  @Command
  fun openFile(invoke: Invoke) {
    try {
      val file = File(invoke.parseArgs(OpenFileArgs::class.java).path)
      val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", file)
      val mime = MimeTypeMap.getSingleton().getMimeTypeFromExtension(file.extension.lowercase()) ?: "*/*"
      val intent = Intent(Intent.ACTION_VIEW).setDataAndType(uri, mime).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
      val ret = JSObject()
      try {
        activity.startActivity(intent)
        ret.put("opened", true)
      } catch (e: ActivityNotFoundException) {
        ret.put("opened", false)
      }
      invoke.resolve(ret)
    } catch (e: Exception) {
      Logger.error(Logger.tags("StoragePlugin"), "无法打开文件", e)
      invoke.reject(e.message ?: e.toString())
    }
  }

  /** 用系统分享面板分享文件：同 openFile 经 FileProvider 换成 content URI，并放进 ClipData，接收方才能取得读取权限 */
  @Command
  fun shareFile(invoke: Invoke) {
    try {
      val file = File(invoke.parseArgs(OpenFileArgs::class.java).path)
      val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", file)
      val intent = Intent(Intent.ACTION_SEND)
        .setType(MimeTypeMap.getSingleton().getMimeTypeFromExtension(file.extension.lowercase()) ?: "*/*")
        .putExtra(Intent.EXTRA_STREAM, uri)
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
      intent.clipData = ClipData.newRawUri(null, uri)
      activity.startActivity(Intent.createChooser(intent, null))
      invoke.resolve()
    } catch (e: Exception) {
      Logger.error(Logger.tags("StoragePlugin"), "无法分享文件", e)
      invoke.reject(e.message ?: e.toString())
    }
  }

  private fun resolveGranted(invoke: Invoke) {
    val ret = JSObject()
    ret.put("granted", Environment.isExternalStorageManager())
    invoke.resolve(ret)
  }

  @Suppress("DEPRECATION")
  private fun primary(): String = Environment.getExternalStorageDirectory().absolutePath

  /** 已挂载的存储卡等卷：UUID → 目录 */
  private fun volumes(): Map<String, String> {
    val sm = activity.getSystemService(StorageManager::class.java) ?: return emptyMap()
    return sm.storageVolumes.mapNotNull { v ->
      val uuid = v.uuid ?: return@mapNotNull null
      val dir = v.directory ?: return@mapNotNull null
      uuid to dir.absolutePath
    }.toMap()
  }

  override fun onResume(activity: AppCompatActivity) {
    BackgroundEvents.post("resume")
  }

  @Command
  fun startIndexing(invoke: Invoke) {
    val args = invoke.parseArgs(ProgressArgs::class.java)
    val ret = JSObject()
    ret.put("started", IndexService.start(activity.applicationContext, args.done, args.total, args.note))
    invoke.resolve(ret)
  }

  @Command
  fun updateIndexing(invoke: Invoke) {
    val args = invoke.parseArgs(ProgressArgs::class.java)
    IndexService.update(activity, args.done, args.total, args.note)
    invoke.resolve()
  }

  @Command
  fun stopIndexing(invoke: Invoke) {
    IndexService.stop(activity.applicationContext)
    invoke.resolve()
  }

  @Command
  fun nextEvent(invoke: Invoke) {
    BackgroundEvents.next(invoke)
  }

  private fun resolveUnrestricted(invoke: Invoke) {
    val pm = activity.getSystemService(PowerManager::class.java)
    val ret = JSObject()
    ret.put("granted", pm?.isIgnoringBatteryOptimizations(activity.packageName) == true)
    invoke.resolve(ret)
  }

  @Command
  fun batteryUnrestricted(invoke: Invoke) {
    resolveUnrestricted(invoke)
  }

  @Command
  fun requestBatteryUnrestricted(invoke: Invoke) {
    val page = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:${activity.packageName}"))
    try {
      startActivityForResult(invoke, page, "unrestrictedResult")
    } catch (e: ActivityNotFoundException) {
      // 个别系统没有这个对话框，退回电池优化的应用列表
      startActivityForResult(invoke, Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS), "unrestrictedResult")
    }
  }

  @ActivityCallback
  fun unrestrictedResult(invoke: Invoke, result: ActivityResult) {
    resolveUnrestricted(invoke)
  }

  /** Android 13 起请求通知权限，不等用户的选择；被拒绝时进度通知不显示，前台服务照常运行 */
  @Command
  fun requestNotifications(invoke: Invoke) {
    if (Build.VERSION.SDK_INT >= 33 && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
      activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
    }
    invoke.resolve()
  }

  private fun resolveCanInstall(invoke: Invoke) {
    val ret = JSObject()
    ret.put("granted", activity.packageManager.canRequestPackageInstalls())
    invoke.resolve(ret)
  }

  /** 是否已在“安装未知应用”中允许本应用 */
  @Command
  fun canInstallPackages(invoke: Invoke) {
    resolveCanInstall(invoke)
  }

  /** 打开本应用的“安装未知应用”设置页（`ACTION_MANAGE_UNKNOWN_APP_SOURCES` 带 package URI），用户返回后报告是否已允许 */
  @Command
  fun requestInstallPackages(invoke: Invoke) {
    val page = Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}"))
    try {
      startActivityForResult(invoke, page, "installPermissionResult")
    } catch (e: ActivityNotFoundException) {
      // 个别系统没有单个应用的设置页，退回所有应用的列表
      startActivityForResult(invoke, Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES), "installPermissionResult")
    }
  }

  @ActivityCallback
  fun installPermissionResult(invoke: Invoke, result: ActivityResult) {
    resolveCanInstall(invoke)
  }

  /** 安装更新 APK（[ApkInstaller]）；写入大文件不占主线程 */
  @Command
  fun installApk(invoke: Invoke) {
    val apk = File(invoke.parseArgs(InstallArgs::class.java).path)
    Thread { ApkInstaller.install(activity, apk, invoke) }.start()
  }

  @Command
  fun allFilesAccess(invoke: Invoke) {
    resolveGranted(invoke)
  }

  @Command
  fun storageRoot(invoke: Invoke) {
    val ret = JSObject()
    ret.put("path", primary())
    invoke.resolve(ret)
  }

  @Command
  fun requestAllFilesAccess(invoke: Invoke) {
    val page = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:${activity.packageName}"))
    try {
      startActivityForResult(invoke, page, "accessResult")
    } catch (e: ActivityNotFoundException) {
      // 个别系统没有单个应用的授权页面，退回所有应用的列表
      startActivityForResult(invoke, Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION), "accessResult")
    }
  }

  @ActivityCallback
  fun accessResult(invoke: Invoke, result: ActivityResult) {
    resolveGranted(invoke)
  }

  @Command
  fun pickFolder(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
    intent.putExtra(DocumentsContract.EXTRA_INITIAL_URI, DocumentsContract.buildDocumentUri(StoragePaths.EXTERNAL, "primary:"))
    startActivityForResult(invoke, intent, "folderResult")
  }

  /** 取消时返回 null；换算不了真实路径（网盘等其他提供者）时返回 `{ unsupported: true }` */
  @ActivityCallback
  fun folderResult(invoke: Invoke, result: ActivityResult) {
    try {
      val uri = result.data?.data
      if (result.resultCode != Activity.RESULT_OK || uri == null) {
        invoke.resolve()
        return
      }
      val path = if (uri.authority == StoragePaths.EXTERNAL) {
        StoragePaths.fromDocumentId(DocumentsContract.getTreeDocumentId(uri), primary(), volumes())
      } else {
        null
      }
      val ret = JSObject()
      if (path == null) ret.put("unsupported", true) else ret.put("path", path)
      invoke.resolve(ret)
    } catch (e: Exception) {
      Logger.error(Logger.tags("StoragePlugin"), "无法读取所选文件夹", e)
      invoke.reject(e.message ?: e.toString())
    }
  }

  @Command
  fun pickFiles(invoke: Invoke) {
    val args = invoke.parseArgs(PickFilesArgs::class.java)
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT)
    intent.addCategory(Intent.CATEGORY_OPENABLE)
    intent.type = if (args.mimeTypes.size == 1) args.mimeTypes[0] else "*/*"
    if (args.mimeTypes.size > 1) intent.putExtra(Intent.EXTRA_MIME_TYPES, args.mimeTypes)
    intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, args.multiple)
    startActivityForResult(invoke, intent, "filesResult")
  }

  /** 取消时返回 null，否则返回 `{ paths }` */
  @ActivityCallback
  fun filesResult(invoke: Invoke, result: ActivityResult) {
    val uris: List<Uri>
    val dir: File
    try {
      val data = result.data
      val clip = data?.clipData
      uris = if (clip != null) (0 until clip.itemCount).map { clip.getItemAt(it).uri } else listOfNotNull(data?.data)
      if (result.resultCode != Activity.RESULT_OK || uris.isEmpty()) {
        invoke.resolve()
        return
      }
      dir = File(invoke.parseArgs(PickFilesArgs::class.java).copyTo)
    } catch (e: Exception) {
      Logger.error(Logger.tags("StoragePlugin"), "无法读取所选文件", e)
      invoke.reject(e.message ?: e.toString())
      return
    }
    // 复制模型这样的大文件要很久，不占主线程
    Thread {
      val copies = mutableListOf<File>()
      try {
        val paths = JSArray()
        for (uri in uris) {
          val path = readablePath(uri) ?: copy(uri, dir).also { copies.add(File(it)) }
          paths.put(path)
        }
        val ret = JSObject()
        ret.put("paths", paths)
        invoke.resolve(ret)
      } catch (e: Exception) {
        // 多选时前面的文件已经复制好，一并删掉
        copies.forEach { it.delete() }
        Logger.error(Logger.tags("StoragePlugin"), "无法读取所选文件", e)
        invoke.reject(e.message ?: e.toString())
      }
    }.start()
  }

  /** 能换算出、且当前可读的真实路径 */
  private fun readablePath(uri: Uri): String? {
    if (!DocumentsContract.isDocumentUri(activity, uri)) return null
    val id = DocumentsContract.getDocumentId(uri)
    val path = when {
      uri.authority == StoragePaths.EXTERNAL -> StoragePaths.fromDocumentId(id, primary(), volumes())
      uri.authority == StoragePaths.DOWNLOADS && id.startsWith("raw:") -> id.substring(4)
      else -> StoragePaths.mediaRef(uri.authority, id)?.let { mediaPath(it) }
    }
    return path?.takeIf { File(it).canRead() }
  }

  /** MediaStore 条目的 `DATA` 列（真实路径）；查不到或没有权限时为空 */
  private fun mediaPath(ref: StoragePaths.MediaRef): String? {
    val base = when (ref.table) {
      StoragePaths.MediaTable.FILES -> MediaStore.Files.getContentUri("external")
      StoragePaths.MediaTable.IMAGES -> MediaStore.Images.Media.EXTERNAL_CONTENT_URI
      StoragePaths.MediaTable.VIDEO -> MediaStore.Video.Media.EXTERNAL_CONTENT_URI
      StoragePaths.MediaTable.AUDIO -> MediaStore.Audio.Media.EXTERNAL_CONTENT_URI
    }
    return try {
      @Suppress("DEPRECATION")
      activity.contentResolver.query(ContentUris.withAppendedId(base, ref.id), arrayOf(MediaStore.MediaColumns.DATA), null, null, null)?.use { c ->
        if (c.moveToFirst() && !c.isNull(0)) c.getString(0) else null
      }
    } catch (e: Exception) {
      Logger.warn(Logger.tags("StoragePlugin"), "无法查询所选文件的路径：${e.message}")
      null
    }
  }

  private fun displayName(uri: Uri): String {
    activity.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
      if (c.moveToFirst() && !c.isNull(0)) return c.getString(0)
    }
    return uri.lastPathSegment ?: ""
  }

  /** 按显示名复制到 [dir]，同名时加序号；复制失败时删掉写了一半的文件 */
  private fun copy(uri: Uri, dir: File): String {
    if (!dir.isDirectory && !dir.mkdirs()) throw IOException("无法创建目录 ${dir.absolutePath}")
    val name = displayName(uri).substringAfterLast('/').ifBlank { "file" }
    val file = File(dir, StoragePaths.uniqueName(name) { File(dir, it).exists() })
    try {
      val input = activity.contentResolver.openInputStream(uri) ?: throw IOException("无法打开 $uri")
      input.use { i -> file.outputStream().use { o -> i.copyTo(o) } }
    } catch (e: Exception) {
      file.delete()
      throw e
    }
    return file.absolutePath
  }
}
