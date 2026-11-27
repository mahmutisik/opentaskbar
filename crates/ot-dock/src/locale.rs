//! Dates and times in the user's language, formatted by Windows (NLS) with the .NET patterns OpenTaskbar uses
//! (`HH:mm`, `ddd`, the culture's month-day pattern, `D`), so month and day names come out as in OpenTaskbar.

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::Globalization::{
    DATE_LONGDATE, DATE_SHORTDATE, GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx,
    LOCALE_SMONTHDAY,
};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

/// The local time now.
pub fn now() -> SYSTEMTIME {
    // SAFETY: plain out-parameter call.
    unsafe {
        let mut time = std::mem::zeroed();
        GetLocalTime(&mut time);
        time
    }
}

/// A Unix time as local time (`ToLocalTime`).
pub fn local_from_unix(seconds: i64) -> SYSTEMTIME {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ticks = (seconds + 11_644_473_600).max(0) as u64 * 10_000_000;
    let file = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    // SAFETY: out-parameter calls.
    unsafe {
        let mut utc = std::mem::zeroed();
        let mut local = std::mem::zeroed();
        FileTimeToSystemTime(&file, &mut utc);
        if SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0 {
            return utc;
        }
        local
    }
}

/// A locale for formatting: the app language when it names a culture ("tr-TR"), else the user's locale.
pub struct Locale(Option<Vec<u16>>);

impl Locale {
    pub fn new(language: &str) -> Self {
        let culture = language.trim();
        let named = culture.len() >= 2
            && culture
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-');
        Self((named && !culture.eq_ignore_ascii_case("system")).then(|| wide(culture)))
    }

    fn name(&self) -> *const u16 {
        self.0.as_ref().map_or(std::ptr::null(), |n| n.as_ptr())
    }

    /// A time with a .NET-style picture (`HH:mm`, `hh:mm tt`; Windows reads the same letters).
    pub fn time(&self, time: &SYSTEMTIME, picture: &str) -> String {
        let picture = wide(picture);
        let mut buffer = [0u16; 128];
        // SAFETY: valid strings and buffer length.
        let n = unsafe {
            GetTimeFormatEx(
                self.name(),
                0,
                time,
                picture.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as i32,
            )
        };
        text(&buffer, n)
    }

    /// `ToString("t")`: the culture's short time.
    pub fn short_time(&self, time: &SYSTEMTIME) -> String {
        let mut buffer = [0u16; 64];
        // SAFETY: valid buffer length; no picture means the locale's format.
        let n = unsafe {
            GetTimeFormatEx(
                self.name(),
                windows_sys::Win32::Globalization::TIME_NOSECONDS,
                time,
                std::ptr::null(),
                buffer.as_mut_ptr(),
                buffer.len() as i32,
            )
        };
        text(&buffer, n)
    }

    /// A date with a picture (`ddd`, `d MMM`), or with Windows' short/long date format when `picture` is `None`.
    fn date(&self, time: &SYSTEMTIME, picture: Option<&str>, flags: u32) -> String {
        let picture = picture.map(wide);
        let mut buffer = [0u16; 256];
        // SAFETY: valid strings and buffer length.
        let n = unsafe {
            GetDateFormatEx(
                self.name(),
                if picture.is_some() { 0 } else { flags },
                time,
                picture.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
                buffer.as_mut_ptr(),
                buffer.len() as i32,
                std::ptr::null(),
            )
        };
        text(&buffer, n)
    }

    pub fn date_picture(&self, time: &SYSTEMTIME, picture: &str) -> String {
        self.date(time, Some(picture), 0)
    }

    /// `ToString("d")`.
    pub fn short_date(&self, time: &SYSTEMTIME) -> String {
        self.date(time, None, DATE_SHORTDATE)
    }

    /// `ToString("D")`.
    pub fn long_date(&self, time: &SYSTEMTIME) -> String {
        self.date(time, None, DATE_LONGDATE)
    }

    fn info(&self, kind: u32) -> String {
        let mut buffer = [0u16; 80];
        // SAFETY: valid buffer length.
        let n =
            unsafe { GetLocaleInfoEx(self.name(), kind, buffer.as_mut_ptr(), buffer.len() as i32) };
        text(&buffer, n)
    }

    /// `DateTimeFormat.FirstDayOfWeek`, 0 = Monday .. 6 = Sunday (`LOCALE_IFIRSTDAYOFWEEK`).
    pub fn first_day_of_week(&self) -> u32 {
        self.info(0x100C).trim().parse::<u32>().map_or(0, |d| d % 7)
    }

    /// `GetShortestDayName`: day 0 = Monday .. 6 = Sunday. .NET takes these from ICU's stand-alone narrow
    /// weekdays ("P", "S", "Ç" in Turkish), so Windows' own ICU is asked first; NLS's shortest names
    /// (`LOCALE_SSHORTESTDAYNAME1..7`, "Pt", "Sa") are the fallback.
    pub fn shortest_day_name(&self, day: u32) -> String {
        let culture = self
            .0
            .as_ref()
            .map(|n| String::from_utf16_lossy(&n[..n.len() - 1]));
        icu_narrow_weekday(culture.as_deref(), day % 7).unwrap_or_else(|| self.info(0x60 + day % 7))
    }

    /// `DateTimeFormat.MonthDayPattern` ("d MMMM", "MMMM d").
    pub fn month_day_pattern(&self) -> String {
        let mut buffer = [0u16; 80];
        // SAFETY: valid buffer length.
        let n = unsafe {
            GetLocaleInfoEx(
                self.name(),
                LOCALE_SMONTHDAY,
                buffer.as_mut_ptr(),
                buffer.len() as i32,
            )
        };
        let pattern = text(&buffer, n);
        if pattern.is_empty() {
            "MMMM d".to_owned()
        } else {
            pattern
        }
    }
}

/// `TimeZoneInfo.Local` as the calendar shows it: "UTC+3 · <zone name>" (`FormatLocalTimeZone`).
pub fn time_zone_label() -> String {
    use windows_sys::Win32::System::Time::{
        DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation,
    };
    // SAFETY: plain out-parameter call.
    let (info, kind) = unsafe {
        let mut info: DYNAMIC_TIME_ZONE_INFORMATION = std::mem::zeroed();
        let kind = GetDynamicTimeZoneInformation(&mut info);
        (info, kind)
    };
    // TIME_ZONE_ID_DAYLIGHT.
    let daylight = kind == 2;
    let bias = info.Bias
        + if daylight {
            info.DaylightBias
        } else {
            info.StandardBias
        };
    let offset = -bias;
    let sign = if offset < 0 { '-' } else { '+' };
    let (hours, minutes) = (offset.abs() / 60, offset.abs() % 60);
    let utc = if minutes == 0 {
        format!("UTC{sign}{hours}")
    } else {
        format!("UTC{sign}{hours}:{minutes:02}")
    };
    let name = if daylight {
        &info.DaylightName[..]
    } else {
        &info.StandardName[..]
    };
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let name = String::from_utf16_lossy(&name[..end]);
    if name.trim().is_empty() {
        utc
    } else {
        format!("{utc} \u{b7} {name}")
    }
}

/// ICU's stand-alone narrow weekday name (`UDAT_STANDALONE_NARROW_WEEKDAYS`) through Windows' `icu.dll`
/// (Windows 10 1903 and later); day 0 = Monday.
fn icu_narrow_weekday(culture: Option<&str>, day: u32) -> Option<String> {
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    type Open = unsafe extern "C" fn(
        i32,
        i32,
        *const u8,
        *const u16,
        i32,
        *const u16,
        i32,
        *mut i32,
    ) -> *mut core::ffi::c_void;
    type Symbols =
        unsafe extern "C" fn(*const core::ffi::c_void, i32, i32, *mut u16, i32, *mut i32) -> i32;
    type Close = unsafe extern "C" fn(*mut core::ffi::c_void);
    const UDAT_NONE: i32 = -1;
    const UDAT_DEFAULT: i32 = 2;
    const UDAT_STANDALONE_NARROW_WEEKDAYS: i32 = 15;
    // ICU locales use underscores; the user's locale when the app follows the system.
    let name = culture.map(|c| c.replace('-', "_")).unwrap_or_default();
    let locale: Vec<u8> = name.bytes().chain(Some(0)).collect();
    let library: Vec<u16> = "icu.dll".encode_utf16().chain(Some(0)).collect();
    // SAFETY: icu.dll from System32 with its documented C API; the format is closed before returning.
    unsafe {
        let module = LoadLibraryExW(
            library.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if module.is_null() {
            return None;
        }
        let open: Open = std::mem::transmute(GetProcAddress(module, c"udat_open".as_ptr().cast())?);
        let symbols: Symbols =
            std::mem::transmute(GetProcAddress(module, c"udat_getSymbols".as_ptr().cast())?);
        let close: Close =
            std::mem::transmute(GetProcAddress(module, c"udat_close".as_ptr().cast())?);
        let mut status = 0i32;
        let format = open(
            UDAT_NONE,
            UDAT_DEFAULT,
            if name.is_empty() {
                std::ptr::null()
            } else {
                locale.as_ptr()
            },
            std::ptr::null(),
            -1,
            std::ptr::null(),
            -1,
            &mut status,
        );
        if format.is_null() || status > 0 {
            return None;
        }
        // ICU numbers the days 1 = Sunday .. 7 = Saturday.
        let index = (day + 1) % 7 + 1;
        let mut buffer = [0u16; 32];
        let mut status = 0i32;
        let n = symbols(
            format,
            UDAT_STANDALONE_NARROW_WEEKDAYS,
            index as i32,
            buffer.as_mut_ptr(),
            buffer.len() as i32,
            &mut status,
        );
        close(format);
        (status <= 0 && n > 0).then(|| String::from_utf16_lossy(&buffer[..n as usize]))
    }
}

/// `CultureInfo.NativeName` (`in_locale` = the culture itself) or `EnglishName` (`in_locale` = "en") as .NET takes
/// them from ICU: "Türkçe (Türkiye)", "español (España)".
pub fn culture_display_name(culture: &str, in_locale: &str) -> Option<String> {
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    type DisplayName = unsafe extern "C" fn(*const u8, *const u8, *mut u16, i32, *mut i32) -> i32;
    let id: Vec<u8> = culture.replace('-', "_").bytes().chain(Some(0)).collect();
    let display: Vec<u8> = in_locale.replace('-', "_").bytes().chain(Some(0)).collect();
    let library: Vec<u16> = "icu.dll".encode_utf16().chain(Some(0)).collect();
    // SAFETY: icu.dll from System32 with its documented C API.
    unsafe {
        let module = LoadLibraryExW(
            library.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if module.is_null() {
            return None;
        }
        let name: DisplayName = std::mem::transmute(GetProcAddress(
            module,
            c"uloc_getDisplayName".as_ptr().cast(),
        )?);
        let mut buffer = [0u16; 128];
        let mut status = 0i32;
        let n = name(
            id.as_ptr(),
            display.as_ptr(),
            buffer.as_mut_ptr(),
            buffer.len() as i32,
            &mut status,
        );
        (status <= 0 && n > 0).then(|| String::from_utf16_lossy(&buffer[..n as usize]))
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The text of a buffer filled with `n` characters including the terminator.
fn text(buffer: &[u16], n: i32) -> String {
    if n <= 1 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..(n - 1) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> SYSTEMTIME {
        SYSTEMTIME {
            wYear: 2026,
            wMonth: 11,
            wDayOfWeek: 2,
            wDay: 24,
            wHour: 20,
            wMinute: 7,
            wSecond: 5,
            wMilliseconds: 0,
        }
    }

    #[test]
    fn formats_like_dotnet() {
        let en = Locale::new("en-US");
        assert_eq!(en.time(&day(), "HH:mm"), "20:07");
        assert_eq!(en.time(&day(), "HH:mm:ss"), "20:07:05");
        assert_eq!(en.time(&day(), "hh:mm tt"), "08:07 PM");
        assert_eq!(en.date_picture(&day(), "ddd"), "Tue");
        assert_eq!(en.month_day_pattern(), "MMMM d");
        assert_eq!(en.date_picture(&day(), "MMM d"), "Nov 24");
        let tr = Locale::new("tr-TR");
        assert_eq!(tr.month_day_pattern(), "d MMMM");
        assert_eq!(tr.date_picture(&day(), "d MMM"), "24 Kas");
        assert_eq!(tr.date_picture(&day(), "ddd"), "Sal");
        assert!(tr.long_date(&day()).contains("Kasım"));
        assert_eq!(tr.first_day_of_week(), 0);
        assert_eq!(en.first_day_of_week(), 6);
        // ICU's narrow names, as .NET shows them.
        assert_eq!(en.shortest_day_name(0), "M");
        assert_eq!(tr.shortest_day_name(2), "Ç");
        assert_eq!(
            culture_display_name("tr-TR", "tr-TR").as_deref(),
            Some("Türkçe (Türkiye)")
        );
        assert_eq!(
            culture_display_name("es-ES", "es-ES").as_deref(),
            Some("español (España)")
        );
        assert_eq!(
            culture_display_name("de-DE", "en").as_deref(),
            Some("German (Germany)")
        );
    }
}
