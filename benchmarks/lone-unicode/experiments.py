#!/usr/bin/env python3
"""Size, fixed-width, and four-input controls for repeated mixed text."""
import json
import pathlib
import sys
from cases import function, mixed


def cases():
    out = []
    for size in [4000, 10000, 20000, 40000, 80000, 160000, 3000000]:
        text = mixed(size)
        out.append(dict(name=f'size_{size}', source=function('input.length', 'int', 'string'),
                        args=[text], expected=len(text), accounting=True,
                        iterations=max(40, 120_000_000 // size)))
    for character in 'é界🙂':
        text = mixed(40000, character=character)
        out.append(dict(name=f'width_{len(character.encode())}', source=function('input.length', 'int', 'string'),
                        args=[text], expected=len(text), accounting=True, iterations=3000))
    texts = [mixed(40000, seed=seed) for seed in range(188, 192)]
    out.append(dict(name='four_inputs_40000',
                    source=function('total=0\ninput.each { |text| total+=text.length }\ntotal', 'int', 'array<string>'),
                    args=[texts], expected=sum(map(len, texts)), accounting=True, iterations=750))
    return out


if __name__ == '__main__':
    pathlib.Path(sys.argv[1]).write_text(json.dumps(cases(), ensure_ascii=False) + '\n')
