#!/bin/bash
# Run only on the explicitly selected server, after source upload and local verification.
set -euo pipefail
cd "$(dirname "$0")/../.."
test "$(uname -m)" = x86_64
test -x target/release/fusion-report-server
sudo -n true
if ! id fusion-reports >/dev/null 2>&1; then
    sudo useradd --system --home /var/lib/fusion-reports --shell /usr/sbin/nologin fusion-reports
fi
sudo install -d -m 0755 /opt/fusion-reports /var/www/fusion-acme
sudo install -d -m 0700 -o fusion-reports -g fusion-reports /var/lib/fusion-reports
sudo install -d -m 0750 -o root -g fusion-reports /etc/fusion-reports
sudo install -m 0755 target/release/fusion-report-server /opt/fusion-reports/fusion-report-server.new
sudo systemctl stop fusion-reports.service 2>/dev/null || true
sudo mv /opt/fusion-reports/fusion-report-server.new /opt/fusion-reports/fusion-report-server
if ! sudo test -f /etc/fusion-reports/admin.key; then
    sudo /opt/fusion-reports/fusion-report-server init-key
    sudo chown root:fusion-reports /etc/fusion-reports/admin.key
    sudo chmod 0640 /etc/fusion-reports/admin.key
fi
sudo install -m 0644 deploy/reports/fusion-reports.service deploy/reports/fusion-reports-backup.service deploy/reports/fusion-reports-backup.timer /etc/systemd/system/
sudo install -m 0644 deploy/reports/nginx-http.conf /etc/nginx/sites-available/fusion-reports-http
sudo ln -sfn /etc/nginx/sites-available/fusion-reports-http /etc/nginx/sites-enabled/fusion-reports-http
# Only the vendor default symlink installed on this freshly reimaged target is removed.
if [ -L /etc/nginx/sites-enabled/default ]; then sudo unlink /etc/nginx/sites-enabled/default; fi
sudo nginx -t
sudo systemctl daemon-reload
sudo systemctl enable --now fusion-reports.service fusion-reports-backup.timer nginx.service
sudo systemctl reload nginx
curl --fail --silent --show-error --retry 8 --retry-connrefused --retry-delay 1 --max-time 10 http://127.0.0.1:18880/health
printf '\nFUSION_RECEIVER_READY\n'
