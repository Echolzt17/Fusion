"""Verify the bundled native kernel using only temporary paths and loopback fixtures.

No real subscription, node addresses, accounts, request bodies, or credentials are read.
"""
import asyncio
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile

EXPECTED_SHA256 = "926bfea2ce48a247d6a7ce4cf641980e416202d5e40fe4ee5c949089ace0e197"


def available_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


async def main(binary):
    binary = Path(binary).resolve()
    if hashlib.sha256(binary.read_bytes()).hexdigest() != EXPECTED_SHA256:
        raise RuntimeError("pinned kernel digest mismatch")
    release = asyncio.Event()
    hits = {"primary": 0, "backup": 0, "direct": 0}
    passwords = {"route-a": "fixture-a-password", "route-b": "fixture-b-password", "orphan": "fixture-orphan-password"}

    async def proxy(reader, writer, name):
        try:
            header = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), 3)
            # Internal route identities must not escape to a provider's proxy.
            assert b"fixture-a-password" not in header and b"fixture-b-password" not in header
            for user, password in passwords.items():
                assert base64.b64encode(f"{user}:{password}".encode()) not in header
            if header.startswith(b"CONNECT "):
                writer.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                await writer.drain()
                header = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), 3)
            hits[name] += 1
            writer.write(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
            data = name.encode()
            writer.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
            await writer.drain()
            if name == "primary" and hits[name] == 1:
                await release.wait()
            writer.write(b"4\r\ndone\r\n0\r\n\r\n")
            await writer.drain()
        except (ConnectionError, asyncio.IncompleteReadError):
            pass
        finally:
            writer.close()
            await writer.wait_closed()

    async def direct(reader, writer):
        hits["direct"] += 1
        writer.close()
        await writer.wait_closed()

    primary = await asyncio.start_server(lambda r, w: proxy(r, w, "primary"), "127.0.0.1", 0)
    backup = await asyncio.start_server(lambda r, w: proxy(r, w, "backup"), "127.0.0.1", 0)
    origin = await asyncio.start_server(direct, "127.0.0.1", 0)
    proxy_port, controller_port = available_port(), available_port()
    while controller_port == proxy_port:
        controller_port = available_port()
    secret = "fixture-controller-secret-0123456789"
    config = {
        "port": 0, "mixed-port": 0, "socks-port": 0,
        "allow-lan": False, "bind-address": "127.0.0.1", "mode": "rule", "log-level": "silent",
        "external-controller": f"127.0.0.1:{controller_port}", "secret": secret,
        "tun": {"enable": False}, "dns": {"enable": False},
        "listeners": [{"name": "fusion-http", "type": "http", "listen": "127.0.0.1", "port": proxy_port,
                       "users": [{"username": user, "password": password} for user, password in passwords.items()]}],
        "proxies": [{"name": name, "type": "http", "server": "127.0.0.1", "port": server.sockets[0].getsockname()[1]}
                    for name, server in [("primary", primary), ("backup", backup)]],
        "rules": ["IN-USER,route-a,primary", "IN-USER,route-b,backup", "MATCH,REJECT"],
    }

    async def controller(auth):
        reader, writer = await asyncio.open_connection("127.0.0.1", controller_port)
        writer.write((f"GET /version HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n").encode())
        await writer.drain()
        response = await asyncio.wait_for(reader.read(), 2)
        writer.close()
        await writer.wait_closed()
        return response

    async def request(user, target="http://fixture.invalid/stream"):
        reader, writer = await asyncio.open_connection("127.0.0.1", proxy_port)
        auth = ""
        if user:
            encoded = base64.b64encode(f"{user}:{passwords.get(user, 'invalid')}".encode()).decode()
            auth = f"Proxy-Authorization: Basic {encoded}\r\n"
        writer.write((f"GET {target} HTTP/1.1\r\nHost: fixture.invalid\r\n{auth}Connection: close\r\n\r\n").encode())
        await writer.drain()
        return reader, writer

    writers = []
    with tempfile.TemporaryDirectory(prefix="fusion-network-native-") as temporary:
        config_path = Path(temporary) / "config.json"
        config_path.write_text(json.dumps(config), encoding="utf-8")
        child = subprocess.Popen([str(binary), "-d", temporary, "-f", str(config_path)],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                 creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        try:
            for _ in range(100):
                if child.poll() is not None:
                    raise RuntimeError("native kernel rejected the fixture")
                try:
                    response = await controller(f"Authorization: Bearer {secret}\r\n")
                    if b"200 OK" in response.split(b"\r\n", 1)[0]:
                        body = json.loads(response.split(b"\r\n\r\n", 1)[1])
                        assert body["version"] == "v1.19.33"
                        break
                except (OSError, ValueError):
                    pass
                await asyncio.sleep(0.1)
            else:
                raise RuntimeError("native kernel readiness deadline")
            assert b"401" in (await controller("")).split(b"\r\n", 1)[0]
            live, writer = await request("route-a")
            writers.append(writer)
            initial_header = await asyncio.wait_for(live.readuntil(b"\r\n\r\n"), 3)
            assert b"200" in initial_header.split(b"\r\n", 1)[0], initial_header.split(b"\r\n", 1)[0].split()[1:2]
            assert b"primary" in await asyncio.wait_for(live.readuntil(b"primary\r\n"), 3)
            fresh, writer = await request("route-b")
            writers.append(writer)
            assert b"backup" in await asyncio.wait_for(fresh.read(), 3)
            # A logical request retaining route-a still reaches primary after the
            # application's future selection has moved to route-b.
            pinned, writer = await request("route-a")
            writers.append(writer)
            assert b"primary" in await asyncio.wait_for(pinned.read(), 3)
            for user in [None, "unknown", "orphan"]:
                denied, writer = await request(user, f"http://127.0.0.1:{origin.sockets[0].getsockname()[1]}/blocked")
                writers.append(writer)
                response = await asyncio.wait_for(denied.read(), 3)
                assert b"200" not in response.split(b"\r\n", 1)[0]
            assert not live.at_eof(), "switch closed the original stream"
            release.set()
            assert b"done" in await asyncio.wait_for(live.read(), 3)
            assert hits == {"primary": 2, "backup": 1, "direct": 0}, hits
            print("NATIVE_NETWORK_VERIFIED authenticated identities pinned nodes; old stream completed; unknown identities and direct fallback rejected; no duplicated business request")
        finally:
            release.set()
            for writer in writers:
                writer.close()
                await writer.wait_closed()
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
            for server in [primary, backup, origin]:
                server.close()
                await server.wait_closed()


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parents[1] / "resources" / "network-kernel" / "mihomo.exe"))
