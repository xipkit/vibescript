#!/usr/bin/env python3
"""Generate timestamp expectations with exact Python rational arithmetic."""

import fractions
import json
import math
import pathlib
import random
import struct


def generate():
    values = [0.0, -0.0, 0.29, -0.29, 0.123456789, -0.123456789,
              999999.9999999999, 1000000.0, 9223372036.854776, -9223372036.854776,
              math.inf, -math.inf, math.nan]
    for exponent in [-1074, -1022, -53, -30, -10, 0, 20, 30, 33, 40, 53, 62, 63, 100, 1023]:
        value = math.ldexp(1.0, exponent)
        for sign in [-1, 1]:
            for toward in [-math.inf, 0, math.inf]:
                values.append(math.nextafter(sign * value, toward))
    rng = random.Random(0x74696D65)
    for _ in range(64):
        values.append(math.ldexp(rng.uniform(-2, 2), rng.randrange(-60, 64)))
    unique = dict.fromkeys(struct.unpack('>Q', struct.pack('>d', value))[0] for value in values)
    cases = []
    for bits in unique:
        value = struct.unpack('>d', struct.pack('>Q', bits))[0]
        for method, scale in [('microseconds', 1000), ('milliseconds', 1000000),
                              ('nanoseconds', 1), ('calendar', 1000),
                              ('addition', 1000000000), ('subtraction', -1000000000)]:
            expected = None
            if math.isfinite(value):
                nanos = math.floor(fractions.Fraction(value) * scale)
                if -2**63 <= nanos < 2**63:
                    if method == 'calendar':
                        if value >= 0 and 0 <= nanos < 10**9:
                            expected = [1704067200, nanos]
                    else:
                        expected = [nanos // 10**9, nanos % 10**9]
            cases.append(dict(method=method, factor_bits=bits, expected=expected))
    return cases


if __name__ == '__main__':
    target = pathlib.Path(__file__).resolve().parents[1] / 'tests' / 'time-float-cases.json'
    target.write_text(json.dumps(generate(), indent=2) + '\n')
