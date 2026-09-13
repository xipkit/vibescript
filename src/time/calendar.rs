// Calendar conversion adapts time/time.go from the Go standard library.
// Copyright 2009 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt.

pub(super) const UNIX_TO_INTERNAL: i64 = 62_135_596_800;
const ABSOLUTE_YEARS: u64 = 292_277_022_400;
const UNIX_TO_ABSOLUTE: u64 =
    (ABSOLUTE_YEARS / 400 * 146097 + 306) * 86400 + UNIX_TO_INTERNAL as u64;

#[derive(Clone, Copy, Debug)]
pub(super) struct Civil {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub weekday: i64,
    pub yearday: i64,
}

fn date_to_days(year: i64, month: i64, day: i64) -> u64 {
    let jan_feb = u64::from(month < 3);
    let month = month as u64 + 12 * jan_feb;
    let year = (year as u64)
        .wrapping_sub(jan_feb)
        .wrapping_add(ABSOLUTE_YEARS);
    let yearday = (979 * month - 2919) >> 5;
    let century = year / 100;
    let cyear = year % 100;
    let century_days = 146097u64.wrapping_mul(century) / 4;
    century_days
        .wrapping_add(1461 * cyear / 4 + yearday)
        .wrapping_add(day as u64)
        .wrapping_sub(1)
}

pub(super) fn normalized(parts: [i64; 6]) -> i64 {
    let [mut year, month, mut day, mut hour, mut minute, second] = parts;
    let month = month.wrapping_sub(1);
    year = year.wrapping_add(month.div_euclid(12));
    let month = month.rem_euclid(12) + 1;
    minute = minute.wrapping_add(second.div_euclid(60));
    let second = second.rem_euclid(60);
    hour = hour.wrapping_add(minute.div_euclid(60));
    let minute = minute.rem_euclid(60);
    day = day.wrapping_add(hour.div_euclid(24));
    let hour = hour.rem_euclid(24);
    date_to_days(year, month, day)
        .wrapping_mul(86400)
        .wrapping_add((hour * 3600 + minute * 60 + second) as u64)
        .wrapping_sub(UNIX_TO_ABSOLUTE) as i64
}

pub(super) fn civil(seconds: i64, offset: i32) -> Civil {
    let absolute = seconds
        .wrapping_add(i64::from(offset))
        .wrapping_add(UNIX_TO_ABSOLUTE as i64) as u64;
    let days = absolute / 86400;
    let d = 4 * days + 3;
    let century = d / 146097;
    let cd = (d % 146097) as u32 | 3;
    let product = 2939745 * u64::from(cd);
    let cyear = product >> 32;
    let ayday = u64::from(product as u32 / 2939745 / 4);
    let md = 2141 * ayday + 197913;
    let jan_feb = u64::from(ayday >= 306);
    let leap = u64::from(cyear % 4 == 0 && (cyear != 0 || century % 4 == 0));
    Civil {
        year: (century * 100).wrapping_sub(ABSOLUTE_YEARS) as i64 + cyear as i64 + jan_feb as i64,
        month: (md >> 16) as i64 - 12 * jan_feb as i64,
        day: 1 + ((md & 0xffff) / 2141) as i64,
        hour: (absolute % 86400 / 3600) as i64,
        minute: (absolute % 3600 / 60) as i64,
        second: (absolute % 60) as i64,
        weekday: ((days + 3) % 7) as i64,
        yearday: ayday as i64 + 60 + (leap & !jan_feb) as i64 - 365 * jan_feb as i64,
    }
}

pub(super) fn leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

pub(super) fn days_before(month: i64) -> i64 {
    [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334][month as usize - 1]
}

pub(super) fn days_in(year: i64, month: i64) -> i64 {
    if month == 2 {
        28 + i64::from(leap(year))
    } else {
        30 + ((month + (month >> 3)) & 1)
    }
}
