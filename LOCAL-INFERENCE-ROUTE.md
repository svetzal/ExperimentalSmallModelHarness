# Isolated local inference route

The opt-in Pier adapter in `benchmarks/local_inference_environment.py` removes
Squid from the local inference data path. Existing adapters and frozen
experiments continue to use their registered topology.

## Topology

The worker stays exclusively on `pier-egress-internal`, an internal Docker
network. A dedicated TCP bridge joins that network and Pier's default network.
It listens only on explicitly registered ports and forwards each port to the
same port on `host.docker.internal`. It does not parse HTTP, accept CONNECT,
choose destinations from request content, or publish ports to the host.

Squid remains available for existing filtered outbound access. Its policy
explicitly denies `host.docker.internal` and the bridge service, even if the
original allowlist contains the host. The worker's `OLLAMA_HOST` and/or
`OMLX_HOST` are rewritten to `harness-local-inference` on the registered port;
only that service and loopback bypass HTTP proxies. Caller-supplied wildcard
proxy exclusions are discarded.

## Opt-in integration

For a new experiment, use `LocalInferenceDockerEnvironment` as the environment
base and pass these explicit constructor arguments:

- `local_inference_ports`: a tuple of the exact provider or capture-proxy ports.
  For an Ollama capture endpoint on 11436, register only `(11436,)`.
- `local_inference_image`: an immutable local Docker image ID in the form
  `sha256:` plus 64 lowercase hexadecimal digits. The image must provide
  `python3` and must already be available locally.

Supply credential-free HTTP provider endpoints using `host.docker.internal`
and those exact ports. Endpoint paths are retained. Authentication headers and
provider keys retain their existing delivery mechanisms; credentials in endpoint
URLs are rejected. Remote hosts, unregistered ports, and missing explicit
endpoints fail closed.

Freeze the adapter, bridge script, image ID, ports, rewritten endpoints, and
network topology in the new experiment registration. Copy both modules into the
frozen adapter packet when needed; the environment module finds the bridge
script beside itself. Do not change registered historical adapters in place.

The bridge drops all Linux capabilities, uses a read-only filesystem, disallows
privilege escalation, and limits processes/threads. Its connect timeout is ten
seconds before any request is forwarded. Once connected, it has no stream or
idle deadline. It preserves bytes and TCP half-closes; it does not retry requests,
replay tools, unload models, or establish generation ownership. Recovery and
memory/ownership safeguards remain separate responsibilities of the harness
and experiment coordinator.

## Validation

Run unit checks with Pier's Python environment:

```sh
python -m unittest discover -s benchmarks \
  -p test_local_inference_environment.py -v
```

The retained no-inference Docker validation is under
`Experiments/LocalInferenceRoute-2026-10-12/` in the enclosing workspace. It used
the actual adapter to generate the isolated topology and a cached bridge image.
All 24 synthetic requests succeeded across two simulated provider endpoints,
including four incrementally delivered streams. Payloads were exact. Negative
checks rejected unregistered ports, direct access to a neighboring host port,
general outbound IP access, and local/general disallowed requests through Squid.
All owned containers and networks were removed. All 149 registered Generation51
hashes remained unchanged.

This establishes route behavior under synthetic traffic. It does not establish
that Squid caused every historical failure or that real inference is more
reliable. The TCP bridge is still an additional connection hop, with a fixed
destination and no HTTP proxy policy or buffering. No real inference was run.
