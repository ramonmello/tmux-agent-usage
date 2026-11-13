//! Local wall-clock time for unix timestamps, through the C library's
//! `localtime_r` with libc's target-specific types and ABI bindings.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub year: i32,
    /// 1–12
    pub month: u32,
    /// 1–31
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    /// 0 = Sunday
    pub weekday: u32,
}

impl Local {
    pub fn at(timestamp: i64) -> Option<Local> {
        // libc deprecates time_t on musl only to announce a future width
        // change; it is still the type localtime_r takes on every target.
        #[allow(deprecated)]
        let timestamp = libc::time_t::try_from(timestamp).ok()?;
        let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
        // SAFETY: libc supplies the target-specific time_t and struct tm.
        // Both pointers remain valid; localtime_r initializes tm on success.
        let result = unsafe { libc::localtime_r(&timestamp, tm.as_mut_ptr()) };
        if result.is_null() {
            return None;
        }
        let tm = unsafe { tm.assume_init() };
        Some(Local {
            year: tm.tm_year.checked_add(1900)?,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            weekday: tm.tm_wday as u32,
        })
    }

    fn same_day(&self, other: &Local) -> bool {
        (self.year, self.month, self.day) == (other.year, other.month, other.day)
    }
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// `today 18:20`, `tomorrow 09:00` or `Sat 14/11 05:00`.
pub fn date_time(timestamp: i64, now: i64) -> String {
    let (Some(at), Some(today)) = (Local::at(timestamp), Local::at(now)) else {
        return "?".to_owned();
    };
    let clock = format!("{:02}:{:02}", at.hour, at.minute);
    if at.same_day(&today) {
        return format!("today {clock}");
    }
    if Local::at(now + 24 * 60 * 60).is_some_and(|tomorrow| at.same_day(&tomorrow)) {
        return format!("tomorrow {clock}");
    }
    format!(
        "{} {:02}/{:02} {clock}",
        WEEKDAYS[at.weekday as usize % 7],
        at.day,
        at.month
    )
}

/// `15:42` when `timestamp` is today, `Mon 09/11 15:42` otherwise.
pub fn short_time(timestamp: i64, now: i64) -> String {
    match (Local::at(timestamp), Local::at(now)) {
        (Some(at), Some(today)) if at.same_day(&today) => {
            format!("{:02}:{:02}", at.hour, at.minute)
        }
        _ => date_time(timestamp, now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_epoch() {
        let local = Local::at(0).unwrap();
        assert!(local.year == 1970 || local.year == 1969);
    }

    #[test]
    fn labels_today_and_tomorrow() {
        let now = 1_794_240_000;
        assert!(
            date_time(now + 60, now).starts_with("today ")
                || date_time(now + 60, now).starts_with("tomorrow ")
        );
        assert!(date_time(now + 10 * 86400, now).contains('/'));
    }
}
