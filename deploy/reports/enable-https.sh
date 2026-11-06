#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
sudo test -f /etc/letsencrypt/live/reports.example.com/fullchain.pem
sudo install -m 0644 nginx-https.conf /etc/nginx/sites-available/fusion-reports-https
sudo ln -sfn /etc/nginx/sites-available/fusion-reports-https /etc/nginx/sites-enabled/fusion-reports-https
sudo nginx -t
sudo systemctl reload nginx
sudo install -d -m 0755 /etc/letsencrypt/renewal-hooks/deploy
printf '%s\n' '#!/bin/sh' 'set -eu' '/usr/sbin/nginx -t' '/usr/bin/systemctl reload nginx' | sudo tee /etc/letsencrypt/renewal-hooks/deploy/fusion-nginx >/dev/null
sudo chmod 0755 /etc/letsencrypt/renewal-hooks/deploy/fusion-nginx
curl --fail --silent --show-error --max-time 10 https://reports.example.com/health
printf '\nFUSION_HTTPS_READY\n'
