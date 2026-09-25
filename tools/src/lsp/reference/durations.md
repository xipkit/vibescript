# Durations

A `duration` is a signed span of whole seconds. Integer unit members build
one, the `Duration` namespace builds and parses them, and durations shift
times.

## Creating durations

`seconds`, `minutes`, `hours`, `days` and `weeks` on an `int` make a duration:

```vibe
reminder = 5.minutes
timeout = 2.hours
delay = 30.seconds
long_wait = 7.days
back = -1.minutes   # a negative duration
```

`Duration.build(*, weeks: number = 0, days: number = 0, hours: number = 0, minutes: number = 0, seconds: number = 0)`
adds named parts and needs at least one. `Duration.parse(text: string)` reads
Go durations such as `"1h30m"` (whole seconds only) and ISO 8601 durations such
as `"PT90S"` or `"P2W"`.

```vibe
Duration.build(hours: 1, minutes: 30).to_i  # 5400
Duration.parse("1h30m").to_i                # 5400
Duration.parse("P2W").days                  # 14
```

## Whole units

Each returns an `int`, truncated toward zero.

### `to_i -> int`

The total number of seconds.

```vibe
2.minutes.to_i  # 120
1.hours.to_i    # 3600
```

### `minutes -> int`

The number of whole minutes.

```vibe
90.seconds.minutes  # 1
2.hours.minutes     # 120
```

### `hours -> int`

The number of whole hours.

```vibe
7200.seconds.hours  # 2
90.minutes.hours    # 1
```

### `days -> int`

The number of whole days.

```vibe
36.hours.days  # 1
```

### `weeks -> int`

The number of whole weeks.

```vibe
14.days.weeks  # 2
20.days.weeks  # 2
```

## Fractional units

Each returns a `float`. Months are 30 days and years 365 days, so `in_months`
and `in_years` are approximations; use times for calendar arithmetic.

- `in_seconds -> float` – the length in seconds.
- `in_minutes -> float` – the length in minutes.
- `in_hours -> float` – the length in hours.
- `in_days -> float` – the length in days.
- `in_weeks -> float` – the length in weeks.
- `in_months -> float` – the length in 30-day months.
- `in_years -> float` – the length in 365-day years.

```vibe
90.seconds.in_minutes  # 1.5
36.hours.in_days       # 1.5
30.days.in_months      # 1.0
365.days.in_years      # 1.0
```

## Formatting

### `to_s -> string`

The number of seconds followed by `s`, as string interpolation renders it.

### `iso8601 -> string`

The ISO 8601 form, such as `"PT1H30M"`.

### `parts -> { days: int, hours: int, minutes: int, seconds: int }`

The duration split into days, hours, minutes and seconds.

```vibe
shift = 90.minutes
shift.to_s            # "5400s"
shift.iso8601         # "PT1H30M"
shift.parts           # {days: 0, hours: 1, minutes: 30, seconds: 0}
shift.parts["hours"]  # 1
2.weeks.iso8601       # "P14D"
```

## Arithmetic and comparison

- `duration + duration` and `duration - duration` are durations.
- `duration * number` scales; a float result rounds to the nearest second,
  halves away from zero.
- `duration / number` divides; an integer divisor truncates toward zero and a
  float divisor rounds like `*`.
- `duration / duration` is their ratio, as a `float`.
- `duration % duration` is the remainder, as a duration.

Durations order with `<`, `<=`, `>`, `>=` and `<=>` and compare with `==`.
Arithmetic that would overflow raises.

```vibe
total = 1.hours + 30.minutes     # 5400s
remaining = 2.hours - 15.minutes # 6300s
scaled = 10.seconds * 3          # 30s
halved = 10.seconds / 2          # 5s
rounded = 7.seconds / 2.0        # 4s
ratio = 10.seconds / 4.seconds   # 2.5
rest = 10.seconds % 4.seconds    # 2s
1.hours == 60.minutes            # true
90.minutes > 1.hours             # true
```

- `between?(min: duration, max: duration) -> bool` – whether the duration is
  within the bounds, inclusive.

## Anchoring to times

A duration counts from now with `ago` and `from_now`, or from a given time with
`before` and `after`. Each returns a new time; from now, it is in UTC.

### `ago -> time`

The time this long before now.

### `from_now -> time`

The time this long after now.

### `before(start: time) -> time`

The time this long before `start`.

### `after(start: time) -> time`

The time this long after `start`.

```vibe
start = Time.utc(2024, 1, 1)
5.minutes.before(start).iso8601  # "2023-12-31T23:55:00Z"
2.hours.after(start).iso8601     # "2024-01-01T02:00:00Z"
expires = 30.days.from_now
seen = 5.minutes.ago
```

## Example: scheduling

```vibe
type Event = { id: int, scheduled_at: time }

def schedule_reminder(event: Event, notice: duration) -> { event_id: int, remind_at: time, notice_seconds: int }
  {
    event_id: event["id"],
    remind_at: notice.before(event["scheduled_at"]),
    notice_seconds: notice.to_i
  }
end

event = { id: 123, scheduled_at: Time.parse("2025-01-15T10:00:00Z") }
reminder = schedule_reminder(event, 30.minutes)
reminder["remind_at"].iso8601  # "2025-01-15T09:30:00Z"
```

## Removed spellings

These names still run until the static language is the default, but do not
compile with static types. Each has one replacement.

- `seconds`, `second` – removed; use `to_i`, the total number of seconds.
- `minute` – removed; use `minutes`.
- `hour` – removed; use `hours`.
- `day` – removed; use `days`.
- `week` – removed; use `weeks`.
- `format` – removed; use `to_s`.
- `since` – removed; use `from_now`, or `after(start)` with a time.
- `until` – removed; use `ago`, or `before(start)` with a time.
- `string` – removed; use `to_s`, the seconds followed by `s`.

`after` and `before` without a time are removed too; write `from_now` and
`ago`. So are `ago` and `from_now` with a time; write `before(start)` and
`after(start)`.
