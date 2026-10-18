#!/usr/bin/env python3
"""Subsets the Phosphor fonts to the glyphs listed in src/icons.rs.

    pip install fonttools
    python3 scripts/subset-icons.py path/to/Phosphor.ttf path/to/Phosphor-Fill.ttf

The full fonts come with https://github.com/phosphor-icons/web (MIT) or the egui-phosphor crate.
"""
import re
import sys
from pathlib import Path

from fontTools import subset

here = Path(__file__).resolve().parent.parent
codes = re.findall(r'\\u\{([0-9A-Fa-f]+)\}', (here / "src/icons.rs").read_text())
for src, name in zip(sys.argv[1:3], ["phosphor.ttf", "phosphor-fill.ttf"]):
    out = here / "assets/fonts" / name
    subset.main([src, f"--unicodes={','.join(codes)}", f"--output-file={out}", "--layout-features=", "--no-hinting"])
    print(out, out.stat().st_size)
