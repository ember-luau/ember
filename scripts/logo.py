"""Regenerates the ascii logo in src/art.rs from the brand mark ember.png.

    python3 scripts/logo.py ember.png

Prints the art; paste it into `art::LOGO`. Needs ImageMagick.

The mark is a flame above an open crate. Its alpha channel already carries
every knockout that matters -- the curl inside the flame, and the gap between
the two crate panels -- so the art needs no hand-placed knockout, unlike the
EMBR mark this replaces.

Two spark diamonds float beside the flame. At 26 columns each covers about
one cell, so it renders as a stray punctuation mark rather than a spark, and
`drop_specks` removes both. Same reason the old generator filled in the EMBR
letters: detail below about a cell cannot survive the ramp.
"""

import re
import subprocess
import sys
from collections import deque

# the ramp art.rs shades by density, lightest first
RAMP = [
    (0.10, "."), (0.22, ":"), (0.34, "-"), (0.48, "="), (0.60, "+"),
    (0.74, "*"), (0.86, "#"), (0.96, "%"), (1.01, "@"),
]
# 26 keeps the help beside the logo on an 80-column terminal: 26 columns, the
# 3-column gap, and main::print_root_help's HELP_MIN of 50 come to 79. The
# rows follow from the trimmed mark's 509x828 and the 2:1 terminal cell.
WIDTH, HEIGHT = 26, 21
SUPERSAMPLE = 8
# a component smaller than this share of the largest one is a speck, not a shape
SPECK = 0.05


def load(source: str) -> tuple[list[list[float]], int, int]:
    """The mark's alpha as per-pixel coverage, trimmed and scaled to the grid."""
    wide, tall = WIDTH * SUPERSAMPLE, HEIGHT * SUPERSAMPLE
    dump = subprocess.run(
        ["magick", source, "-background", "none", "-alpha", "on",
         "-trim", "+repage", "-resize", f"{wide}x{tall}!", "-depth", "8", "txt:-"],
        capture_output=True, text=True, check=True,
    ).stdout

    ink = [[0.0] * wide for _ in range(tall)]
    pixel = re.compile(r"^(\d+),(\d+): \((\d+),(\d+),(\d+),(\d+)\)")
    for line in dump.splitlines()[1:]:
        found = pixel.match(line)
        if not found:
            continue
        x, y, _r, _g, _b, a = (int(value) for value in found.groups())
        ink[y][x] = a / 255
    return ink, wide, tall


def drop_specks(ink: list[list[float]], wide: int, tall: int) -> int:
    """Erases components under SPECK of the largest. Returns how many went."""
    seen = [[False] * wide for _ in range(tall)]
    components = []
    for y in range(tall):
        for x in range(wide):
            if ink[y][x] <= 0.5 or seen[y][x]:
                continue
            queue = deque([(x, y)])
            seen[y][x] = True
            cells = []
            while queue:
                cx, cy = queue.popleft()
                cells.append((cx, cy))
                for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                    nx, ny = cx + dx, cy + dy
                    if 0 <= nx < wide and 0 <= ny < tall and not seen[ny][nx] and ink[ny][nx] > 0.5:
                        seen[ny][nx] = True
                        queue.append((nx, ny))
            components.append(cells)

    if not components:
        return 0
    largest = max(len(cells) for cells in components)
    dropped = 0
    for cells in components:
        if len(cells) >= largest * SPECK:
            continue
        dropped += 1
        # the halo the resize leaves around a speck sits below the 0.5 cut,
        # so clearing only the component leaves a smudge. 2px covers it.
        for cx, cy in cells:
            for y in range(max(0, cy - 2), min(tall, cy + 3)):
                for x in range(max(0, cx - 2), min(wide, cx + 3)):
                    ink[y][x] = 0.0
    return dropped


def to_art(ink: list[list[float]]) -> str:
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


def render(source: str) -> str:
    ink, wide, tall = load(source)
    drop_specks(ink, wide, tall)
    return to_art(ink)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    print(render(sys.argv[1]))
