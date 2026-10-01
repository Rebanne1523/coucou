// Local wall-clock time without a date library: the log stamps and the dated
// backup of ~/.claude/settings.json are the only places that need it.

pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

#[cfg(windows)]
pub fn now() -> LocalTime {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    LocalTime {
        year: t.wYear as i32,
        month: t.wMonth as u32,
        day: t.wDay as u32,
        hour: t.wHour as u32,
        minute: t.wMinute as u32,
        second: t.wSecond as u32,
    }
}

#[cfg(unix)]
pub fn now() -> LocalTime {
    // localtime_r, not localtime: the log is written from several threads.
    unsafe {
        let secs = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        LocalTime {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
        }
    }
}
