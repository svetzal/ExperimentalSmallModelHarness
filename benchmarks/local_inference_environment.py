"""Opt-in Pier adapter for local inference without Squid on its data path."""
from __future__ import annotations

import json
import re
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

from pier.environments.docker.docker import DockerEnvironment

SERVICE = "harness-local-inference"


def validate_ports(ports: tuple[int, ...]) -> None:
    if not ports or len(set(ports)) != len(ports):
        raise ValueError("Register a nonempty set of distinct local inference ports")
    if any(type(port) is not int or not 1 <= port <= 65535 for port in ports):
        raise ValueError("Invalid local inference port")


def routed_environment(env: dict[str, str], ports: tuple[int, ...]) -> dict[str, str]:
    """Rewrite only explicitly registered local endpoints; never broaden NO_PROXY."""
    merged = dict(env)
    found = False
    for key in ("OLLAMA_HOST", "OMLX_HOST"):
        if key not in merged:
            continue
        endpoint = urlsplit(merged[key])
        if (endpoint.scheme != "http" or endpoint.hostname != "host.docker.internal"
                or endpoint.port not in ports or endpoint.username is not None
                or endpoint.password is not None or endpoint.query or endpoint.fragment):
            raise ValueError(f"{key} must be a registered credential-free local HTTP endpoint")
        merged[key] = urlunsplit(("http", f"{SERVICE}:{endpoint.port}", endpoint.path, "", ""))
        found = True
    if not found:
        raise ValueError("An explicit local inference endpoint is required")
    # Do not inherit caller-supplied '*' or unrelated host bypasses.
    merged["NO_PROXY"] = merged["no_proxy"] = f"localhost,127.0.0.1,{SERVICE}"
    return merged


def add_bridge(compose: dict, script: Path, image: str, ports: tuple[int, ...]) -> dict:
    validate_ports(ports)
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
        raise ValueError("Pin the bridge's locally available image by SHA256 image ID")
    if compose["services"]["main"].get("networks") != ["pier-egress-internal"]:
        raise ValueError("Worker must remain exclusively on Pier's internal network")
    if not compose["networks"]["pier-egress-internal"].get("internal"):
        raise ValueError("Pier internal network isolation is required")
    compose["services"][SERVICE] = {
        "image": image,
        "entrypoint": ["python3", "/control/local_inference_bridge.py"],
        "command": [str(port) for port in ports],
        "volumes": [f"{script.resolve()}:/control/local_inference_bridge.py:ro"],
        "networks": ["pier-egress-internal", "default"],
        "read_only": True,
        "cap_drop": ["ALL"],
        "security_opt": ["no-new-privileges:true"],
        "pids_limit": 256,
        "healthcheck": {
            "test": ["CMD", "python3", "-c", "import socket; " + "; ".join(
                f"socket.create_connection(('127.0.0.1', {port}), 1).close()" for port in ports)],
            "interval": "1s", "timeout": "5s", "retries": 30,
        },
    }
    compose["services"]["main"].setdefault("depends_on", {})[SERVICE] = {"condition": "service_healthy"}
    return compose


class LocalInferenceDockerEnvironment(DockerEnvironment):
    """Explicitly opt in with pinned bridge image and exact provider/capture ports."""

    def __init__(self, *args, task_env_config, local_inference_ports: tuple[int, ...],
                 local_inference_image: str, **kwargs):
        validate_ports(local_inference_ports)
        self.local_inference_ports = local_inference_ports
        self.local_inference_image = local_inference_image
        config = task_env_config.model_copy(deep=True)
        config.allow_internet = False
        super().__init__(*args, task_env_config=config, **kwargs)

    def _prepare_egress_proxy_compose(self) -> None:
        super()._prepare_egress_proxy_compose()
        if self._egress_proxy_compose_path is None:
            raise ValueError("Local inference requires Pier's filtered isolated topology")
        path = self._egress_proxy_compose_path
        compose = json.loads(path.read_text())
        add_bridge(compose, Path(__file__).with_name("local_inference_bridge.py"),
                   self.local_inference_image, self.local_inference_ports)
        script = self.trial_paths.trial_dir / "egress-proxy" / "start-squid.sh"
        source = script.read_text()
        marker = "http_access deny !Safe_ports"
        if source.count(marker) != 1:
            raise ValueError("Unrecognized Squid policy template")
        # Local routes must never fall back to Squid, even when host is allowlisted.
        source = source.replace(marker, "acl local_inference dstdomain host.docker.internal "
                                + SERVICE + "\nhttp_access deny local_inference\n" + marker)
        script.write_text(source)
        path.write_text(json.dumps(compose, indent=2))

    def agent_process_env(self, env: dict[str, str] | None) -> dict[str, str]:
        return routed_environment(super().agent_process_env(env) or {}, self.local_inference_ports)
