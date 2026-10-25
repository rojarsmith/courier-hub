#!/bin/sh
set -eu
# Replace api.example.com with this site's certificate name when installing.
# Certbot also invokes directory hooks for other managed certificates.
[ "${RENEWED_LINEAGE:-}" = /etc/letsencrypt/live/api.example.com ] || exit 0
/usr/sbin/nginx -t
/usr/bin/systemctl reload nginx
