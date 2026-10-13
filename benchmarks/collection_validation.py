"""Read-only collection checks against a registered Git baseline."""

from pathlib import Path
import subprocess


class OriginalTestsChanged(ValueError):
    """The collected revision changed a test present in the baseline."""


def _test_entries(workspace: Path, revision: str) -> dict[bytes, bytes]:
    # NUL delimiters preserve filenames containing tabs or newlines. Comparing
    # mode/type/object protects content, executable bits and symlink targets.
    tree = subprocess.check_output(
        ["git", "ls-tree", "-r", "-z", revision, "--", "tests/"],
        cwd=workspace,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    return {
        path: metadata
        for entry in tree.split(b"\0")
        if entry
        for metadata, path in [entry.split(b"\t", 1)]
    }


def verify_original_tests(
    workspace: Path, baseline_revision: str, collected_revision: str = "HEAD"
) -> None:
    """Allow new tests; require every baseline test's Git entry to be intact.

    Validate the committed collection revision, as used for the binary patch,
    rather than transient working-tree files. Invalid Git revisions fail closed.
    This does not change or accept any preserved experiment result.
    """
    baseline = _test_entries(workspace, baseline_revision)
    collected = _test_entries(workspace, collected_revision)
    changed = sorted(path for path, entry in baseline.items() if collected.get(path) != entry)
    if changed:
        names = ", ".join(repr(path.decode("utf-8", errors="backslashreplace")) for path in changed)
        raise OriginalTestsChanged(f"Original tests altered: {names}")
