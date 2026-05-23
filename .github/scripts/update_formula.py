#!/usr/bin/env python3
"""Update Formula/bx.rb in place with a new version and per-platform SHA256s.

The formula uses sentinel comments (`# sha256:<platform>`) on each `sha256`
line so this script can find and substitute them without parsing Ruby. If any
sentinel is missing the script aborts loudly — silent drift would publish a
formula pointing at the new release with the old digests.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

FORMULA = Path(__file__).resolve().parents[2] / "Formula" / "bx.rb"

PLATFORMS = ("darwin_arm64", "linux_arm64", "linux_x64")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", required=True)
    for p in PLATFORMS:
        ap.add_argument(f"--{p.replace('_', '-')}", required=True)
    args = ap.parse_args()

    text = FORMULA.read_text()

    text, n = re.subn(
        r'(\n\s*version\s+")[^"]+(")',
        rf"\g<1>{args.version}\g<2>",
        text,
        count=1,
    )
    if n != 1:
        sys.exit("ERROR: could not find `version` line in Formula/bx.rb")

    for p in PLATFORMS:
        digest = getattr(args, p)
        anchor = f"# sha256:{p}"
        pattern = rf'(sha256\s+")[0-9a-fA-F]{{64}}("\s*{re.escape(anchor)})'
        text, n = re.subn(pattern, rf"\g<1>{digest}\g<2>", text, count=1)
        if n != 1:
            sys.exit(f"ERROR: could not find sentinel `{anchor}` in Formula/bx.rb")

    FORMULA.write_text(text)
    print(f"Formula/bx.rb -> version {args.version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
