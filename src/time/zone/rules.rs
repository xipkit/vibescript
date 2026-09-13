// POSIX timezone rules adapt time/zoneinfo.go from the Go standard library.
// Copyright 2009 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt.

use super::{Offset, calendar};
use crate::{CallContext, Result};
use std::ops::Range;

#[derive(Clone, Copy, Debug)]
struct Rule {
    kind: u8,
    day: i64,
    week: i64,
    month: i64,
    time: i64,
}

#[derive(Clone, Debug)]
pub(super) struct Rules {
    standard: Range<usize>,
    daylight: Range<usize>,
    standard_offset: i32,
    daylight_offset: i32,
    transitions: Option<(Rule, Rule)>,
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    end: usize,
    ctx: &'a mut CallContext,
}

impl Parser<'_> {
    fn byte(&self) -> Option<u8> {
        (self.pos < self.end).then(|| self.bytes[self.pos])
    }
    fn eat(&mut self, byte: u8) -> bool {
        if self.byte() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn name(&mut self) -> Result<Option<Range<usize>>> {
        let bracket = self.eat(b'<');
        let start = self.pos;
        while let Some(b) = self.byte() {
            if (self.pos - start) % 1024 == 0 {
                self.ctx.charge(1)?;
                self.ctx.checkpoint()?;
            }
            if bracket && b == b'>' {
                let end = self.pos;
                self.pos += 1;
                return Ok(Some(start..end));
            }
            if !bracket && (b.is_ascii_digit() || matches!(b, b',' | b'-' | b'+')) {
                break;
            }
            self.pos += 1;
        }
        Ok((!bracket && self.pos - start >= 3).then_some(start..self.pos))
    }
    fn number(&mut self, min: i64, max: i64) -> Result<Option<i64>> {
        let start = self.pos;
        let mut n = 0;
        while let Some(b @ b'0'..=b'9') = self.byte() {
            if (self.pos - start) % 1024 == 0 {
                self.ctx.charge(1)?;
                self.ctx.checkpoint()?;
            }
            n = n * 10 + i64::from(b - b'0');
            if n > max {
                return Ok(None);
            }
            self.pos += 1;
        }
        Ok((self.pos != start && n >= min).then_some(n))
    }
    fn offset(&mut self) -> Result<Option<i32>> {
        let negative = if self.eat(b'-') {
            true
        } else {
            self.eat(b'+');
            false
        };
        let Some(hour) = self.number(0, 168)? else {
            return Ok(None);
        };
        let mut value = hour * 3600;
        if self.eat(b':') {
            let Some(minute) = self.number(0, 59)? else {
                return Ok(None);
            };
            value += minute * 60;
            if self.eat(b':') {
                let Some(second) = self.number(0, 59)? else {
                    return Ok(None);
                };
                value += second;
            }
        }
        Ok(Some((if negative { -value } else { value }) as i32))
    }
    fn rule(&mut self) -> Result<Option<Rule>> {
        let mut rule = Rule {
            kind: 0,
            day: 0,
            week: 0,
            month: 0,
            time: 7200,
        };
        if self.eat(b'J') {
            let Some(day) = self.number(1, 365)? else {
                return Ok(None);
            };
            rule.day = day;
        } else if self.eat(b'M') {
            let Some(month) = self.number(1, 12)? else {
                return Ok(None);
            };
            if !self.eat(b'.') {
                return Ok(None);
            }
            let Some(week) = self.number(1, 5)? else {
                return Ok(None);
            };
            if !self.eat(b'.') {
                return Ok(None);
            }
            let Some(day) = self.number(0, 6)? else {
                return Ok(None);
            };
            rule.kind = 2;
            rule.month = month;
            rule.week = week;
            rule.day = day;
        } else {
            let Some(day) = self.number(0, 365)? else {
                return Ok(None);
            };
            rule.kind = 1;
            rule.day = day;
        }
        if self.eat(b'/') {
            let Some(time) = self.offset()? else {
                return Ok(None);
            };
            rule.time = i64::from(time);
        }
        Ok(Some(rule))
    }
}

impl Rule {
    fn seconds(self, year: i64, offset: i32) -> i64 {
        let day = match self.kind {
            0 => self.day - 1 + i64::from(calendar::leap(year) && self.day >= 60),
            1 => self.day,
            _ => {
                let month = (self.month + 9) % 12 + 1;
                let y = year - i64::from(self.month <= 2);
                let (century, cyear) = (y / 100, y % 100);
                let weekday = ((26 * month - 2) / 10 + 1 + cyear + cyear / 4 + century / 4
                    - 2 * century)
                    .rem_euclid(7);
                let mut day = (self.day - weekday).rem_euclid(7);
                for _ in 1..self.week {
                    if day + 7 >= calendar::days_in(year, self.month) {
                        break;
                    }
                    day += 7;
                }
                day + calendar::days_before(self.month)
                    + i64::from(calendar::leap(year) && self.month > 2)
            }
        };
        day * 86400 + self.time - i64::from(offset)
    }
}

impl Rules {
    pub fn parse(
        ctx: &mut CallContext,
        bytes: &[u8],
        start: usize,
        end: usize,
    ) -> Result<Option<Self>> {
        let mut p = Parser {
            bytes,
            pos: start,
            end,
            ctx,
        };
        let Some(standard) = p.name()? else {
            return Ok(None);
        };
        let Some(offset) = p.offset()? else {
            return Ok(None);
        };
        let mut rules = Self {
            standard,
            daylight: 0..0,
            standard_offset: -offset,
            daylight_offset: 0,
            transitions: None,
        };
        if matches!(p.byte(), None | Some(b',')) {
            return Ok(Some(rules));
        }
        let Some(daylight) = p.name()? else {
            return Ok(None);
        };
        rules.daylight = daylight;
        rules.daylight_offset = if matches!(p.byte(), None | Some(b',')) {
            -offset + 3600
        } else {
            let Some(offset) = p.offset()? else {
                return Ok(None);
            };
            -offset
        };
        if p.byte().is_none() {
            rules.transitions = Some((
                Rule {
                    kind: 2,
                    day: 0,
                    week: 2,
                    month: 3,
                    time: 7200,
                },
                Rule {
                    kind: 2,
                    day: 0,
                    week: 1,
                    month: 11,
                    time: 7200,
                },
            ));
        } else {
            if !p.eat(b',') && !p.eat(b';') {
                return Ok(None);
            }
            let Some(start_rule) = p.rule()? else {
                return Ok(None);
            };
            if !p.eat(b',') {
                return Ok(None);
            }
            let Some(end_rule) = p.rule()? else {
                return Ok(None);
            };
            if p.byte().is_some() {
                return Ok(None);
            }
            rules.transitions = Some((start_rule, end_rule));
        }
        Ok(Some(rules))
    }

    pub fn lookup<'a>(&self, bytes: &'a [u8], last: i64, seconds: i64) -> Offset<'a> {
        let mut standard = Offset {
            name: &bytes[self.standard.clone()],
            seconds: self.standard_offset,
            dst: false,
            start: last,
            end: i64::MAX,
        };
        let Some((start_rule, end_rule)) = self.transitions else {
            return standard;
        };
        let mut daylight = Offset {
            name: &bytes[self.daylight.clone()],
            seconds: self.daylight_offset,
            dst: true,
            start: last,
            end: i64::MAX,
        };
        let date = calendar::civil(seconds, 0);
        let ysec = (date.yearday - 1) * 86400 + seconds % 86400;
        let year_start = seconds.wrapping_sub(ysec);
        let mut start = start_rule.seconds(date.year, self.standard_offset);
        let mut end = end_rule.seconds(date.year, self.daylight_offset);
        if end < start {
            std::mem::swap(&mut start, &mut end);
            std::mem::swap(&mut standard, &mut daylight);
        }
        if ysec < start {
            Offset {
                start: year_start,
                end: year_start.wrapping_add(start),
                ..standard
            }
        } else if ysec >= end {
            Offset {
                start: year_start.wrapping_add(end),
                end: year_start.wrapping_add(365 * 86400),
                ..standard
            }
        } else {
            Offset {
                start: year_start.wrapping_add(start),
                end: year_start.wrapping_add(end),
                ..daylight
            }
        }
    }
}
