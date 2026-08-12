"""Regenerates the ascii logo in src/art.rs from the website's lpm-logo.png.

    python3 scripts/logo.py ../lpm-website/public/lpm-logo.png

Prints the art; paste it into `art::LOGO`. The mark's own LPM letters are
knocked out of a shape only 36 columns wide, far too fine to survive that
(they come out as speckle however much they are thickened first), so only the
notch is kept as a knockout and the letters are filled. Needs ImageMagick.
"""

import re
import subprocess
import sys

# the ramp art.rs shades by density, lightest first
RAMP = [
    (0.10, "."), (0.22, ":"), (0.34, "-"), (0.48, "="), (0.60, "+"),
    (0.74, "*"), (0.86, "#"), (0.96, "%"), (1.01, "@"),
]
WIDTH, HEIGHT = 36, 18
# terminal cells are about twice as tall as they are wide, hence 2:1
SUPERSAMPLE = 8
# where the notch sits in the artwork, as a fraction of each axis
NOTCH = (0.56, 0.90, 0.18, 0.52)


def render(source: str) -> str:
    wide, tall = WIDTH * SUPERSAMPLE, HEIGHT * SUPERSAMPLE
    dump = subprocess.run(
        ["magick", source, "-background", "none", "-alpha", "on",
         "-resize", f"{wide}x{tall}!", "-depth", "8", "txt:-"],
        capture_output=True, text=True, check=True,
    ).stdout

    ink = [[0.0] * wide for _ in range(tall)]
    pixel = re.compile(r"^(\d+),(\d+): \((\d+),(\d+),(\d+),(\d+)\)")
    left, right, top, bottom = NOTCH
    for line in dump.splitlines()[1:]:
        found = pixel.match(line)
        if not found:
            continue
        x, y, r, g, b, a = (int(value) for value in found.groups())
        coverage = a / 255
        if left <= x / wide <= right and top <= y / tall <= bottom:
            coverage *= 1 - min(r, g, b) / 255  # the notch is white, knock it out
        ink[y][x] = coverage

    lines = []
    for row in range(HEIGHT):
        cells = [
            sum(ink[row * SUPERSAMPLE + dy][col * SUPERSAMPLE + dx]
                for dy in range(SUPERSAMPLE) for dx in range(SUPERSAMPLE))
            / (SUPERSAMPLE * SUPERSAMPLE)
            for col in range(WIDTH)
        ]
        lines.append("".join(
            " " if value < RAMP[0][0] else next(ch for limit, ch in RAMP if value < limit)
            for value in cells
        ).rstrip())
    while lines and not lines[0]:
        lines.pop(0)
    while lines and not lines[-1]:
        lines.pop()
    return "\n".join(lines)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    print(render(sys.argv[1]))
