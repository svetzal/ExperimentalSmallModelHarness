"""Fixed-destination TCP bridge; never interprets HTTP or chooses a request target."""
from __future__ import annotations

import socket
import threading


def forward(client: socket.socket, port: int) -> None:
    """Preserve bytes and half-closes, without a healthy-stream idle deadline."""
    try:
        upstream = socket.create_connection(("host.docker.internal", port), timeout=10)
        upstream.settimeout(None)
    except OSError:
        client.close()
        return

    def pump(source: socket.socket, target: socket.socket) -> None:
        try:
            while data := source.recv(65536):
                target.sendall(data)
        except OSError:
            pass
        finally:
            try:
                target.shutdown(socket.SHUT_WR)
            except OSError:
                pass

    outbound = threading.Thread(target=pump, args=(client, upstream))
    outbound.start()
    pump(upstream, client)
    outbound.join()
    upstream.close()
    client.close()


def serve(ports: tuple[int, ...]) -> None:
    listeners = []
    # Bind every registered port before starting any accept loop; fail closed.
    for port in ports:
        listener = socket.socket()
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("0.0.0.0", port))
        listener.listen(64)
        listeners.append((listener, port))

    def accept(listener: socket.socket, port: int) -> None:
        while True:
            client, _ = listener.accept()
            threading.Thread(target=forward, args=(client, port), daemon=True).start()

    threads = [threading.Thread(target=accept, args=pair) for pair in listeners]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()


if __name__ == "__main__":
    import sys

    ports = tuple(int(value) for value in sys.argv[1:])
    if not ports or len(set(ports)) != len(ports) or any(not 1 <= p <= 65535 for p in ports):
        raise SystemExit("Expected distinct registered TCP ports")
    serve(ports)
