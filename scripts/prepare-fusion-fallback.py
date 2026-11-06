"""Prepare a local Clash Verge extension from two previously reviewed fingerprints.

Read-only controller metadata. No subscription, credentials, node addresses,
business requests, controller mutation or active connection cleanup.
"""
import argparse
import hashlib
import http.client
import io
import json
import os
from pathlib import Path


def fingerprint(name):
    return hashlib.sha256(name.encode()).hexdigest()[:12]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--selector', required=True)
    parser.add_argument('--primary', required=True)
    parser.add_argument('--backup', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    pipes = [n for n in os.listdir('\\\\.\\pipe\\') if n.startswith('verge-mihomo-production-')]
    if len(pipes) != 1:
        raise ValueError('ambiguous_controller')

    def read(path):
        with open('\\\\.\\pipe\\' + pipes[0], 'r+b', buffering=0) as stream:
            class Socket:
                def makefile(self, *_a, **_k):
                    return io.BufferedReader(stream)
            stream.write(('GET ' + path + ' HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n').encode())
            response = http.client.HTTPResponse(Socket())
            response.begin()
            content = response.read(2 * 1024 * 1024 + 1)
            if response.status != 200 or len(content) > 2 * 1024 * 1024:
                raise ValueError('controller_rejected')
            return json.loads(content)

    proxies = read('/proxies')['proxies']
    groups = [v for name, v in proxies.items() if fingerprint(name) == args.selector and v.get('type') == 'Selector']
    if len(groups) != 1 or fingerprint(groups[0].get('now', '')) != args.primary:
        raise ValueError('current_primary_changed')
    names = []
    for wanted in [args.primary, args.backup]:
        matches = [n for n in groups[0].get('all', []) if fingerprint(n) == wanted]
        if len(matches) != 1 or proxies[matches[0]].get('type') in ['Selector', 'URLTest', 'Fallback', 'Direct', 'Reject', 'Compatible', 'LoadBalance']:
            raise ValueError('candidate_missing_or_not_node')
        names.append(matches[0])
    if names[0] == names[1]:
        raise ValueError('duplicate_candidate')
    source = (Path(__file__).resolve().parents[1] / 'deploy/proxy/fusion-fallback.js').read_text(encoding='utf8')
    source = source.replace('"__FUSION_PRIMARY__"', json.dumps(names[0], ensure_ascii=True)).replace('"__FUSION_BACKUP__"', json.dumps(names[1], ensure_ascii=True))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    # Never overwrite a previously reviewed extension.
    with args.output.open('x', encoding='utf8', newline='\n') as output:
        output.write(source)
    connections = read('/connections').get('connections', [])
    fusion = [c for c in connections if c.get('metadata', {}).get('process', '').lower() == 'personal-codex-pool.exe']
    print(json.dumps({'status': 'prepared_not_applied', 'primaryFingerprint': args.primary,
                      'backupFingerprint': args.backup, 'candidateCount': len(names),
                      'fusionConnectionsObserved': len(fusion), 'mutationPerformed': False}))


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print('{"status":"prepare_failed","rawErrorStored":false,"mutationPerformed":false}')
        raise SystemExit(1)
