#!/usr/bin/env python3
"""Generate exact rational expectations for binary64 duration scaling."""
import json
import math
import random
import struct
from fractions import Fraction
from pathlib import Path


def main():
    rng = random.Random(91304)
    durations = [0, 1, -1, 2, -2, 59, -59, 2**53 + 1, -(2**53 + 1), 2**63 - 1, -2**63, 2**62, -(2**62)]
    factors = [0.0, -0.0, 0.5, -0.5, 1.0, -1.0, 1.5, -1.5, 2.9, 0.1, 0.99,
               math.nextafter(0.5, 0), math.nextafter(0.5, 1), math.nextafter(1, 0),
               math.nextafter(1, 2), 1e-300, 1e300, 5e-324]
    inputs = [(seconds, factor) for seconds in durations for factor in factors]
    inputs += [(rng.randrange(-2**63, 2**63), rng.uniform(-2, 2)) for _ in range(400)]
    inputs += [(rng.randrange(-2**63, 2**63), struct.unpack('>d', rng.getrandbits(64).to_bytes(8, 'big'))[0]) for _ in range(100)]
    cases = []
    for seconds, factor in inputs:
        if not math.isfinite(factor):
            continue
        for divide in [False, True]:
            expected = None
            if not divide or factor:
                exact = Fraction(seconds) / Fraction(factor) if divide else Fraction(seconds) * Fraction(factor)
                whole, remainder = divmod(abs(exact.numerator), exact.denominator)
                whole += 2 * remainder >= exact.denominator
                whole *= -1 if exact < 0 else 1
                if -2**63 <= whole < 2**63:
                    expected = whole
            cases.append({'seconds': seconds,
                          'factor_bits': struct.unpack('>Q', struct.pack('>d', factor))[0],
                          'divide': divide, 'expected': expected})
    path = Path(__file__).resolve().parent.parent / 'tests/duration-float-cases.json'
    path.write_text(json.dumps(cases, indent=2) + '\n')
    print(f'Generated {len(cases)} independent duration scaling cases')


if __name__ == '__main__':
    main()
