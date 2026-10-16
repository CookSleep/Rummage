//! 非 Windows 平台的占位实现，只用于编译和测试：没有 “edit” 动词，调用方改为在文件管理器中显示；查不到默认程序，按普通方式打开；时间按 UTC 计算；不能强制结束进程。
use std::io;

/// Android 上打开文件交给其他应用（`android::ui`），不用它
#[cfg(not(target_os = "android"))]
pub fn edit(_path: &std::path::Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "没有编辑动词"))
}

/// 没有查默认程序的接口，打开到命中位置时一律按普通方式打开
pub fn default_app(_ext: &str) -> Option<std::path::PathBuf> {
    None
}

pub fn os_version() -> String {
    std::env::consts::OS.to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn kill(_pid: u32) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "不支持结束进程"))
}

/// 没有时区接口时用 UTC
pub fn next_midnight() -> io::Result<i64> {
    Ok((now_secs() / 86_400 + 1) as i64 * 86_400_000)
}

#[cfg(desktop)]
pub fn local_time_at(ms: i64) -> io::Result<[u16; 6]> {
    Ok(civil(ms.max(0) as u64 / 1000))
}

pub fn local_time() -> [u16; 6] {
    civil(now_secs())
}

/// Unix 秒换算成 UTC 的 `[年, 月, 日, 时, 分, 秒]`
fn civil(secs: u64) -> [u16; 6] {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // 由 1970-01-01 起的天数换算公历日期（Howard Hinnant 的 civil_from_days）
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    [
        year as u16,
        month as u16,
        day as u16,
        (rest / 3600) as u16,
        (rest % 3600 / 60) as u16,
        (rest % 60) as u16,
    ]
}
