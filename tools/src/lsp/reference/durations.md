# Duration Methods

Vibescript provides duration literals and helper methods for time-based calculations.

## Creating Durations

Duration literals can be created using numeric values with time unit suffixes:

```vibe
reminder = 5.minutes
timeout = 2.hours
delay = 30.seconds
long_wait = 7.days
```

Supported units: `second`/`seconds`, `minute`/`minutes`, `hour`/`hours`, `day`/`days`, `week`/`weeks`

## Duration Methods

### `seconds`

Returns the total duration in seconds as an integer:

```vibe
(2.minutes).seconds  # 120
(1.hour).seconds     # 3600
```

### `minutes`

Returns the duration converted to minutes, truncated to a whole number:

```vibe
(90.seconds).minutes  # 1 (truncated from 1.5)
(2.hours).minutes     # 120
```

**Note:** This method truncates fractional minutes. Use `.seconds / 60` if you need fractional values.

### `hours`

Returns the duration converted to hours, truncated to a whole number:

```vibe
(7200.seconds).hours  # 2
(90.minutes).hours    # 1 (truncated from 1.5)
```

**Note:** This method truncates fractional hours. Use `.seconds / 3600` if you need fractional values.

### `format`

Returns a string representation of the duration:

```vibe
(2.hours).format    # "7200s"
(30.seconds).format # "30s"
```

### `weeks`

Returns the duration converted to weeks, truncated to a whole number:

```vibe
(14.days).weeks   # 2
(20.days).weeks   # 2 (truncated from 2.857)
```

### `in_months`, `in_years`

These helpers return floating-point approximations using 30-day months and 365-day years:

```vibe
(30.days).in_months  # 1.0
(365.days).in_years  # 1.0
```

For precise calendar math, prefer working with exact dates.

## Arithmetic

Durations support basic arithmetic operations:

```vibe
total = 1.hour + 30.minutes      # 5400 seconds
remaining = 2.hours - 15.minutes # 6300 seconds
scaled = 10.seconds * 3          # 30 seconds
divided = 10.seconds / 2         # 5 seconds
ratio = 10.seconds / 4.seconds   # 2.5
mod = 10.seconds % 4.seconds     # 2 seconds
```

## Comparison

`eql?` is a predicate that compares two durations for exact equality. It returns
`true` only when both operands are equal `Duration` values, `false` for an
unequal `Duration` or a non-`Duration` operand, and raises only when given the
wrong number of arguments.

```vibe
1.hour.eql?(60.minutes)            # true
1.hour.eql?(2.hours)               # false
1.hour.eql?(1)                     # false (wrong type)
1.hour.eql?(Time.utc(2024, 1, 1))  # false (wrong type)
```

Like every value, durations also answer the universal `equal?` identity
predicate; for an immutable duration it matches `eql?`. See
[Universal Predicates](stdlib_core_utilities.md#universal-predicates).

## Example: Scheduling

```vibe
def schedule_reminder(event, advance_notice)
  event_time = event[:scheduled_at]
  reminder_time = event_time - advance_notice

  {
    event_id: event[:id],
    reminder_at: reminder_time,
    advance_seconds: advance_notice.seconds
  }
end

schedule_reminder(
  { id: 123, scheduled_at: "2025-01-15T10:00:00Z" },
  30.minutes
)
```
