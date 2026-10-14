"""Exercise a copied release executable while its build checkout is unavailable to Git."""
import http.server
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading

binary = Path(sys.argv[1]).resolve()
revision = sys.argv[2]
source = Path(__file__).resolve().parents[1]
requests = []


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        body = b'data: {"choices":[{"delta":{"content":"DONE"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n'
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


with tempfile.TemporaryDirectory(prefix="microgentic-installed-") as temporary:
    base = Path(temporary)
    installed = base / "bin" / "microgentic"
    installed.parent.mkdir()
    shutil.copy2(binary, installed)
    root = base / "project"
    root.mkdir()
    contract = base / "contract.json"
    contract.write_text(json.dumps(dict(profile=dict(id="terminal_work", version="terminal_work_profile.v1"),
                                        guidance="Reply DONE without tools.", probes=[])))
    git = shutil.which("git")
    assert git, "Git is needed by the smoke fixture"
    wrapper = installed.parent / "git"
    wrapper.write_text('#!/bin/sh\nif [ "$1" = "-C" ] && [ "$2" = ' + shlex.quote(str(source)) +
                       ' ]; then exit 97; fi\nexec ' + shlex.quote(git) + ' "$@"\n')
    wrapper.chmod(0o755)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    env = dict(os.environ, PATH=str(installed.parent) + os.pathsep + os.environ["PATH"],
               OMLX_HOST=f"http://127.0.0.1:{server.server_port}", OMLX_API_KEY="fixture-key",
               XDG_CONFIG_HOME=str(base / "config"), XDG_STATE_HOME=str(base / "state"))
    try:
        version = subprocess.check_output([str(installed), "--version"], env=env, text=True)
        assert version.startswith("microgentic ")
        subprocess.run([str(installed), "--help"], env=env, check=True, stdout=subprocess.DEVNULL)
        output = subprocess.run([str(installed), "benchmark", "run", "--experiment", str(base),
                                 "--contract", str(contract), "--max-iterations", "2",
                                 "--max-model-interactions", "2"], cwd=root, env=env,
                                input=b"unrelated stdin", capture_output=True, timeout=30)
        assert output.returncode == 0, output.stderr
        summary = json.loads(output.stdout)
        provenance = summary["harness_source_state"]
        assert provenance["git_head"] == revision, provenance
        assert provenance["git_dirty"] is False, provenance
        assert provenance["source_state_note"] == "release revision embedded at build time", provenance
        assert summary["final_summary"] == "DONE"
        assert len(requests) == 1
        assert not (base / "state").exists()
        if os.name == "posix":
            subprocess.run([sys.executable, str(source / "tests/tui_smoke.py"), str(installed), str(base / "tui")],
                           env=env, check=True, timeout=30)
        print(json.dumps(dict(version=version.strip(), revision=revision, installed_binary_smoke="passed")))
    finally:
        server.shutdown()
        server.server_close()
