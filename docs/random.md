# Random values and identifiers

`rand`, `srand`, `uuid` and `random_id` are global functions. They reject keywords and blocks. Seeded generator state belongs to one script call.

## Random numbers

`rand` returns a float in `[0, 1)`. `rand(n)` returns an integer from zero through `n - 1`; the bound must be a positive signed 64-bit integer.

`rand(range)` accepts bounded integer ranges, including descending ranges and the complete signed 64-bit interval. Exclusive ranges omit their written end. Empty and open-ended ranges are errors.

```vibe
srand(42)
first = [rand, rand(10), rand(1..3)]
previous = srand(42)
[first == [rand, rand(10), rand(1..3)], previous] # [true, 42]
```

`srand(seed)` takes a signed 64-bit integer and returns the previous seed, or `nil` before the first seed in that call, so its result is an `int?`. Omitting the seed, or passing `nil`, obtains a new seed from the entropy reader. Repeating a seed reproduces the same sequence, including mixed float, bounded-integer and wide-range draws, as Go v0.70.0 did. Calls and threads keep independent seeded state.

Seeded `rand` is predictable and unsuitable for secrets. Unseeded `rand` reads entropy directly.

## UUIDs and tokens

`uuid` returns a lowercase, hyphenated UUID with a Unix-millisecond timestamp and version/variant bits defined by [RFC 9562 version 7](https://www.rfc-editor.org/rfc/rfc9562.html#name-uuid-version-7).

```vibe
id = uuid
[id.length, id[14], id[19]&.match?("[89ab]")]
# [36, "7", true]
```

`random_id(length = 16)` returns an alphanumeric ASCII token. Length must be an integer between 1 and 1024. Rejection sampling gives every character the same probability.

```vibe
token = random_id(8)
[token.length, token.match?("^[a-zA-Z0-9]+$")] # [8, true]
```

UUIDs and tokens always use entropy, even after `srand`, and do not advance the seeded sequence.

## Host configuration

By default the engine reads OS entropy through [getrandom](https://docs.rs/getrandom/0.4.3/getrandom/). Hosts can supply a reader for subsequently compiled scripts:

```rust
use vibescript::{CallOptions, Engine};

fn main() -> Result<(), vibescript::Error> {
    let mut engine = Engine::new();
    // Fixed entropy is useful for tests.
    engine.set_random_source(|ctx, output| {
        ctx.checkpoint()?;
        output.fill(0);
        Ok(output.len())
    });
    let script = engine.compile("random_id(8)")?;
    let output = script.run(CallOptions::default())?;
    assert_eq!(output.value.as_bytes(), Some(b"aaaaaaaa".as_slice()));
    Ok(())
}
```

The callback returns the number of initialized bytes. The engine retries partial reads and rejects zero or oversized counts. Reader errors propagate. A reader may run concurrently; it owns synchronization of any captured state and must cooperate with the supplied context while doing blocking work. Reconfiguring the engine does not alter already compiled scripts.

The comparison harness supports an optional `entropy_byte` fixture field. Only fixtures that specify it use repeated fixed bytes. This makes entropy-dependent conformance and accounting comparisons reproducible; native tests also exercise the real OS reader.

## Resource limits

Token and UUID output is checked against the memory quota before entropy is requested. Token lengths above 1024 raise a latched output-limit error. A token reader that supplies no acceptable bytes for nine consecutive complete reads fails.

Seeded state uses 607 accounted 64-bit words and reuses its allocation when reseeded. It is released when the script call ends, before retained-memory statistics are reported. Failed reseeding does not leak storage.

Seeding, sampling, partial reads and rejection retries consume logical work. Cancellation, deadlines and prior exhaustion are checked before and after each entropy callback; an ignored quota failure remains latched. These checks are cooperative and do not interrupt a blocking host callback or OS call.
