fn main() {
    // 不向前端开放命令，只登记 Android 工程
    tauri_plugin::Builder::new(&[])
        .android_path("android")
        .build();
}
