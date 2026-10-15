"""Package checked release binaries with immutable source and artifact receipts."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import tomllib

root = Path(__file__).resolve().parents[1]
tag, platform, revision = sys.argv[1:4]
version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
assert tag == "v" + version, "release tag must match Cargo.toml"
subprocess.run([sys.executable, str(root / "scripts" / "release-notes.py"), tag], check=True, stdout=subprocess.DEVNULL)
assert platform in ("darwin-arm64", "darwin-x64", "linux-x64")
assert subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip() == revision
assert not subprocess.check_output(["git", "status", "--porcelain"], cwd=root), "release checkout must be clean"
out = root / "dist"
out.mkdir(exist_ok=True)
target = Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
if not target.is_absolute():
    target = root / target
with tempfile.TemporaryDirectory(prefix="microgentic-package-") as temporary:
    base = Path(temporary)
    receipts = {}
    for name in ("microgentic", "adaptive-agent-harness"):
        binary = target / "release" / name
        assert version in subprocess.check_output([str(binary), "--version"], text=True)
        receipts[name] = hashlib.sha256(binary.read_bytes()).hexdigest()
    manifest = base / "release.json"
    manifest.write_text(json.dumps(dict(schema_version="microgentic_release.v1", version=version,
                                        revision=revision, platform=platform, binaries=receipts,
                                        mojentic_revision="4ca1ed279c02eab37827a1ed07c30e961155ecf3"), indent=2) + "\n")
    archive = out / f"microgentic-{platform}.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        for name in receipts:
            tar.add(target / "release" / name, arcname=name)
        tar.add(root / "LICENSE", arcname="LICENSE")
        tar.add(manifest, arcname="release.json")
    print(json.dumps(dict(archive=str(archive), sha256=hashlib.sha256(archive.read_bytes()).hexdigest())))
