#!/usr/bin/env python3
"""Scripted public-worker regression: quiet external inference resumes safely."""
import argparse
import concurrent.futures
import hashlib
import http.server
import json
import os
import platform
from pathlib import Path
import subprocess
import sys
import threading
import time
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'recovery'))
from worker_conformance import frame


def check(worker, root, provider, pause):
    root.mkdir(parents=True, exist_ok=False)
    workspace = root / 'workspace'
    workspace.mkdir()
    (root / 'task.md').write_text('Run the supplied tool once, then finish with DONE.\n')
    requests = []
    host = 'linux-x86_64' if os.environ.get('PAUSE_WORKER_PORTABLE') else ('macos-aarch64' if platform.system() == 'Darwin' else 'linux-x86_64')
    (root / 'model-throughput.jsonl').write_text(json.dumps({
        'timestamp': '2026-10-10T00:00:00Z', 'model': 'synthetic-model', 'provider': provider,
        'host_signature': host, 'context_band': 'unknown', 'packet_type': 'fixture',
        'expected_output_tokens': 4096, 'turn': 1, 'llm_call_depth': 0,
        'prompt_eval_count': None, 'prompt_eval_duration_ns': None,
        'eval_count': 10000, 'eval_duration_ns': 1000000000, 'tokens_per_second': 10000.0}) + '\n')
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_POST(self):
            requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(200)
            self.send_header('Content-Type', 'application/x-ndjson' if provider == 'ollama' else 'text/event-stream')
            self.end_headers()
            def send(chunk):
                self.wfile.write(chunk)
                self.wfile.flush()
            if len(requests) == 1:
                send(frame(provider, 'reasoning', 'Preserve this native reasoning.'))
                call = {'function': {'name': 'shell_command', 'arguments': {'command': "printf 'ONCE\\n' >> executions.txt; printf 'TOOL_RESULT_SURVIVES'"}}}
                if provider == 'omlx':
                    call.update(index=0, id='call_once', type='function')
                    call['function']['arguments'] = json.dumps(call['function']['arguments'])
                send(frame(provider, tools=[call]))
                # Synthetic fast throughput reduces the projection to its normal
                # warmup/slack floor (~361s); the subsequent pause exceeds it.
                terminal = frame(provider, done=True)
                if provider == 'ollama':
                    value = json.loads(terminal)
                    value.update(eval_count=10000, eval_duration=1000000000)
                    terminal = json.dumps(value).encode() + b'\n'
                else:
                    terminal = terminal.replace(b'data: [DONE]', b'data: {"choices":[],"usage":{"completion_tokens":10000},"timings":{"predicted_per_second":10000}}\n\ndata: [DONE]')
                send(terminal)
            else:
                send(frame(provider, 'reasoning', 'Before the external stream pause.'))
                time.sleep(pause)
                send(frame(provider, 'reasoning', 'After the external stream pause.'))
                send(frame(provider, 'content', 'DONE'))
                send(frame(provider, done=True))
    server = http.server.ThreadingHTTPServer(('0.0.0.0', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    env = {k: os.environ[k] for k in ('PATH', 'HOME', 'TMPDIR') if k in os.environ}
    env.update({provider.upper() + '_HOST': f'http://127.0.0.1:{server.server_port}', 'HARNESS_PROGRESS_STATUS_INTERVAL_SECONDS': '1'})
    command = [str(worker), 'run', '--experiment', str(root), '--provider', provider,
               '--model', 'synthetic-model', '--max-iterations', '1', '--max-model-interactions', '2',
               '--preserve-reasoning', '--reasoning-effort', 'enabled', '--recover-requests',
               '--recovery-max-attempts', '3', '--recovery-capture-dir', str(root / 'captures'),
               '--max-thinking-only-tokens', '0', '--repair-exit-thinking-tokens', '0']
    start = time.monotonic()
    try:
        result = subprocess.run(command, cwd=workspace, env=env, capture_output=True, timeout=pause + 180)
    finally:
        server.shutdown()
        server.server_close()
    (root / 'stdout.txt').write_bytes(result.stdout)
    (root / 'stderr.txt').write_bytes(result.stderr)
    (root / 'requests.json').write_text(json.dumps(requests, indent=2))
    assert result.returncode == 0, result.stderr.decode()
    assert len(requests) == 2
    assert (workspace / 'executions.txt').read_text() == 'ONCE\n'
    assert 'TOOL_RESULT_SURVIVES' in json.dumps(requests[1]['messages'])
    assert 'Preserve this native reasoning.' in json.dumps(requests[1]['messages'])
    events = [json.loads(line) for path in (root / 'traces').glob('*.jsonl') for line in path.read_text().splitlines()]
    statuses = [e['payload'] for e in events if e['kind'] == 'llm.progress.status']
    unknown = [p for p in statuses if p['progress_state'] == 'ProgressUnknown' and p['projected_allowance_exceeded']]
    assert unknown, 'Did not exercise unknown activity beyond throughput projection'
    assert not any(e['kind'] == 'llm.progress.interrupted' for e in events)
    assert sum(e['kind'] == 'tool.shell_command' for e in events) == 1
    attempts = [e for e in events if e['kind'] == 'llm.recovery.lifecycle' and e['payload'].get('transition') == 'attempt_started']
    assert len(attempts) == 2, [e['kind'] for e in events if 'recovery' in e['kind']]
    receipt = {'passed': True, 'provider': provider, 'pause_seconds': pause, 'runtime_seconds': time.monotonic() - start,
               'worker_sha256': hashlib.sha256(worker.read_bytes()).hexdigest(), 'logical_requests': 2,
               'wire_attempts': 2, 'tool_executions': 1, 'unknown_beyond_projection_checks': len(unknown), 'real_inference': False}
    (root / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--pause-seconds', type=float, default=390)
    args = parser.parse_args()
    assert args.pause_seconds >= 390
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        receipts = list(pool.map(lambda provider: check(args.worker.resolve(), args.artifacts.resolve() / provider, provider, args.pause_seconds), ('ollama', 'omlx')))
    print(json.dumps(receipts, indent=2))

if __name__ == '__main__': main()
