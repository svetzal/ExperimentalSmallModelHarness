"""Print a version's CHANGELOG section as GitHub release notes; fail if it is missing."""
from pathlib import Path
import re
import sys

INSTALL = """
Install with `brew install svetzal/tap/microgentic`, or upgrade with
`brew upgrade svetzal/tap/microgentic`.
"""


def section(changelog: str, version: str) -> str:
    heading = re.compile(r"^## \[" + re.escape(version) + r"\][^\n]*\n", re.M)
    start = heading.search(changelog)
    if not start:
        raise SystemExit(f"CHANGELOG.md has no section for {version}")
    end = re.compile(r"^(## \[|\[[^\]]+\]: )", re.M).search(changelog, start.end())
    body = changelog[start.end():end.start() if end else len(changelog)].strip()
    if not body:
        raise SystemExit(f"CHANGELOG.md section for {version} is empty")
    return body


if __name__ == "__main__":
    tag = sys.argv[1]
    root = Path(__file__).resolve().parents[1]
    print(section((root / "CHANGELOG.md").read_text(), tag.removeprefix("v")))
    print(INSTALL, end="")
