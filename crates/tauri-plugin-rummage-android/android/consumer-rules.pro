# EmbedProcess 由 Rust 侧经 JNI 按类名调用（src-tauri/src/android/embed.rs），R8 看不到这些调用，混淆后会找不到类
-keep class com.cooksleep.rummage.storage.EmbedProcess { *; }
