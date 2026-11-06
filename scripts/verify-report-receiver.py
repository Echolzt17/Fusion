"""Exercise a built report receiver in an isolated database with public fixtures."""
import gzip
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid


def fixture(version):
    at = int(time.time())
    report = dict(schemaVersion=version, reportId=str(uuid.uuid4()),
        installationId='00000000-0000-4000-8000-000000000001', preparedAt=at,
        rangeFrom=at-86400, rangeTo=at, historyLimited=False,
        environment=dict(appVersion='1.0.6', os='Windows 11', osBuild=26100,
            architecture='x64', webview2Version=None, desktopVersion='26.1027.3384.0',
            engineVersion='0.166.0-alpha.5.1', uptimeSeconds=120),
        errors=[dict(requestId=str(uuid.uuid4()), at=at, operation='responses',
            code='upstream_error', elapsedMs=1000, accountAlias=None,
            conversationAlias=None, appVersion='1.0.6', upstreamStatus=503,
            failureStage='upstream_rejected', networkErrorKind=None,
            proxySource='windows_static_configured', execution=None)])
    if version >= 2:
        report['errors'][0]['details'] = dict(responseType='json', detectedResponseType=None,
            terminalEvent=None, upstreamFailure='server')
    if version == 3:
        report['errors'][0]['details']['origin'] = 'upstream_service'
        report['reliability'] = dict(coverageStart=at, snapshotAt=at, historyPartial=True,
            buckets=[dict(hour=at//3600*3600, appVersion='1.0.6', operation='responses',
                success=2, failure=1, cancelled=0, stopped=0, reconnectStarted=1,
                reconnectRecovered=1, streamInterrupted=0, failuresByOrigin=[0,0,1,0,0],
                elapsedHistogram=[3,0,0,0,0,0], successBySize=[0,2,0,0,0],
                failureBySize=[0,1,0,0,0])])
    return report


def main(binary):
    with tempfile.TemporaryDirectory(prefix='fusion-receiver-verify-') as temporary:
        root = Path(temporary)
        key = 'a' * 64  # Public fixture, never a production credential.
        (root/'fixture-admin').write_text(key)
        with socket.socket() as bound:
            bound.bind(('127.0.0.1', 0)); port = bound.getsockname()[1]
        env = dict(os.environ, FUSION_REPORT_DATA=str(root/'data'),
            FUSION_REPORT_ADMIN_FILE=str(root/'fixture-admin'), FUSION_REPORT_PORT=str(port))
        child = subprocess.Popen([str(Path(binary).resolve()), 'serve'], env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        base = f'http://127.0.0.1:{port}'
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def call(path, report=None, authorized=False):
            headers = {'Authorization': 'Bearer '+key} if authorized else {}
            data = None
            if report is not None:
                headers['Content-Type'] = 'application/gzip'
                data = gzip.compress(json.dumps(report).encode())
            req = urllib.request.Request(base+path, data=data, headers=headers)
            try:
                with opener.open(req, timeout=5) as response:
                    content=response.read()
                    return response.status, content.decode() if path=='/health' else json.loads(content)
            except urllib.error.HTTPError as error:
                return error.code, json.load(error)
        try:
            for _ in range(100):
                if child.poll() is not None: raise RuntimeError('receiver exited')
                try:
                    if call('/health')[0] == 200: break
                except urllib.error.URLError: time.sleep(.1)
            else: raise RuntimeError('receiver startup timed out')
            for version in [1, 2, 3]:
                report = fixture(version)
                status, receipt = call('/v1/error-reports', report)
                assert status == 200 and receipt['acceptedErrors'] == 1
                assert call('/v1/error-reports', report) == (status, receipt)
            report = fixture(3); report['errors'] = []
            status, receipt = call('/v1/error-reports', report)
            assert status == 200 and receipt['acceptedErrors'] == receipt['duplicateErrors'] == 0
            assert call('/v1/error-reports', report) == (status, receipt)
            assert call('/v1/admin/reliability')[0] == 401
            status, page = call('/v1/admin/reliability', authorized=True)
            assert status == 200 and len(page['buckets']) == 1
            assert page['buckets'][0]['bucket']['success'] == 2
            invalid = fixture(3); invalid['errors'][0]['code'] = 'success'
            assert call('/v1/error-reports', invalid)[0] == 400
            print('ISOLATED_RECEIVER_VERIFIED v1 v2 v3 summary-only receipts auth overlap invalid-report')
        finally:
            child.terminate()
            try: child.wait(timeout=5)
            except subprocess.TimeoutExpired: child.kill(); child.wait()


if __name__ == '__main__':
    main(sys.argv[1])
