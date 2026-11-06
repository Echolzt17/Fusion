"""Isolated native Mihomo test: fallback affects new connections, not live streams.

Only loopback fake proxies and temporary config. No real subscription or credentials.
"""
import asyncio
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile


def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


async def main(binary):
    unhealthy = asyncio.Event()
    release = asyncio.Event()
    primary_hits = 0
    backup_hits = 0

    async def proxy(reader, writer, primary):
        nonlocal primary_hits, backup_hits
        try:
            header = await reader.readuntil(b'\r\n\r\n')
            line = header.split(b'\r\n', 1)[0]
            if line.startswith(b'CONNECT '):
                writer.write(b'HTTP/1.1 200 Connection Established\r\n\r\n')
                await writer.drain()
                header = await reader.readuntil(b'\r\n\r\n')
                line = header.split(b'\r\n', 1)[0]
            if b'/check' in line:
                status = '503 Unavailable' if primary and unhealthy.is_set() else '204 No Content'
                writer.write(f'HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n'.encode())
                await writer.drain()
            elif b'/stream' in line:
                if primary: primary_hits += 1
                else: backup_hits += 1
                writer.write(b'HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n')
                writer.write(b'7\r\nprimary\r\n' if primary else b'6\r\nbackup\r\n')
                await writer.drain()
                if primary: await release.wait()
                writer.write(b'4\r\ndone\r\n0\r\n\r\n')
                await writer.drain()
            else:
                raise AssertionError('unexpected fixture request')
        except (ConnectionError, asyncio.IncompleteReadError):
            pass
        finally:
            writer.close()
            await writer.wait_closed()

    first = await asyncio.start_server(lambda r,w:proxy(r,w,True), '127.0.0.1', 0)
    second = await asyncio.start_server(lambda r,w:proxy(r,w,False), '127.0.0.1', 0)
    with tempfile.TemporaryDirectory(prefix='fusion-fallback-verify-') as temporary:
        mixed, controller = port(), port()
        config = {'mixed-port': mixed, 'bind-address': '127.0.0.1', 'allow-lan': False,
            'mode':'rule', 'log-level':'silent', 'external-controller':f'127.0.0.1:{controller}',
            'proxies':[{'name':name, 'type':'http', 'server':'127.0.0.1', 'port':server.sockets[0].getsockname()[1]}
                       for name,server in [('primary',first),('backup',second)]],
            'proxy-groups':[{'name':'fixture-fallback','type':'fallback','proxies':['primary','backup'],
                'url':'http://127.0.0.1/check','expected-status':204,'interval':1,'timeout':300,
                'lazy':False,'empty-fallback':'REJECT'}], 'rules':['MATCH,fixture-fallback']}
        file = Path(temporary)/'fixture.json';file.write_text(json.dumps(config))
        child = subprocess.Popen([str(Path(binary).resolve()), '-d', temporary, '-f', str(file)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)

        async def state():
            reader,writer = await asyncio.open_connection('127.0.0.1', controller)
            writer.write(b'GET /proxies/fixture-fallback HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n')
            await writer.drain()
            response = await asyncio.wait_for(reader.read(), 2)
            writer.close();await writer.wait_closed()
            return json.loads(response.split(b'\r\n\r\n',1)[1])['now']

        async def stream():
            reader,writer = await asyncio.open_connection('127.0.0.1', mixed)
            writer.write(b'GET http://127.0.0.1/stream HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n')
            await writer.drain()
            header = await asyncio.wait_for(reader.readuntil(b'\r\n\r\n'), 3)
            assert header.split(b'\r\n',1)[0].split()[1]==b'200', header.split(b'\r\n',1)[0]
            return reader,writer
        live_writer = None
        try:
            for _ in range(100):
                if child.poll() is not None: raise RuntimeError('native core rejected fixture')
                try:
                    if await state()=='primary':break
                except (OSError,ValueError):pass
                await asyncio.sleep(.1)
            else:raise RuntimeError('native core startup deadline')
            live,live_writer = await stream()
            assert await live.readuntil(b'primary\r\n') == b'7\r\nprimary\r\n'
            unhealthy.set()
            for _ in range(100):
                if await state()=='backup':break
                await asyncio.sleep(.1)
            else:raise RuntimeError('fallback did not switch')
            fresh,fresh_writer = await stream()
            assert b'backup' in await asyncio.wait_for(fresh.read(),3)
            fresh_writer.close();await fresh_writer.wait_closed()
            assert not live.at_eof(), 'existing stream closed during switch'
            release.set()
            assert b'done' in await asyncio.wait_for(live.read(),3)
            assert primary_hits==backup_hits==1
            print('NATIVE_FALLBACK_VERIFIED primary stream completed; new connection used backup; one request per stream')
        finally:
            release.set()
            if live_writer:
                live_writer.close();await live_writer.wait_closed()
            child.terminate()
            try:child.wait(timeout=5)
            except subprocess.TimeoutExpired:child.kill();child.wait()
            first.close();second.close()
            await first.wait_closed();await second.wait_closed()


if __name__=='__main__':
    asyncio.run(main(sys.argv[1]))
