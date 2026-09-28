#!/usr/bin/env python3
"""Record independent Python binary64 expectations for numeric string conversion."""
import json
import math
import random
import struct
from pathlib import Path


def generate():
    rng = random.Random(170913)
    inputs = []
    for _ in range(400):
        width = rng.randrange(1, 200)
        digits = "".join(rng.choice("0123456789abcdef") for _ in range(width))
        point = rng.randrange(width + 1)
        exponent = rng.randrange(-1500, 1500)
        inputs.append(rng.choice(["", "-", "+"]) + "0x" + digits[:point] + "." + digits[point:] + "p" + str(exponent))
    rng = random.Random(913)
    for _ in range(200):
        inputs.append(rng.choice(["", "-", "+"]) + "".join(rng.choice("0123456789") for _ in range(rng.randrange(1, 400))) + "e" + str(rng.randrange(-1500, 1000)))
    inputs += [
        "0x1.00000000000008p0", "0x1.000000000000080000000000001p0",
        "0x1.fffffffffffff8p0", "0x1p-1075", "-0x1p-1075",
        "0x1.00000000000000000000000001p-1075", "0x1.fffffffffffffp-1023",
        "0x1.fffffffffffff8p1023", "0x1.fffffffffffffp1023",
    ]
    records = []
    for text in inputs:
        try:
            value = float.fromhex(text) if "0x" in text else float(text)
        except OverflowError:
            value = math.inf
        bits = struct.unpack(">Q", struct.pack(">d", value))[0] if math.isfinite(value) else None
        records.append({"input": text, "bits": bits})
    return records


if __name__ == "__main__":
    path = Path(__file__).resolve().parent.parent / "tests/float-conversions.json"
    path.write_text(json.dumps(generate(), indent=2) + "\n")
