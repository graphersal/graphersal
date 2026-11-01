#!/bin/bash
# First-boot setup of the DigitalOcean droplet that serves the static sites of Graphersal:
#   graphersal.dev        the book (mdBook output), plus /schema/v1 (the schema meta-schema)
#   play.graphersal.dev   the WebAssembly playground
#   www.*, graphersal.com, graphersal.org   permanent redirects to https://graphersal.dev
#
# Paste it into "Startup Scripts" (user data) when creating the droplet (Ubuntu 24.04 LTS, with an
# SSH key). It runs once, as root; its log is /var/log/cloud-init-output.log.
#
# Afterwards:
#   1. Point the DNS A (and AAAA) records of the names above at the droplet.
#   2. Run `sudo graphersal-certs` on the droplet: it requests HTTPS certificates for every name
#      that already resolves to this droplet; run it again when more names resolve.
#   3. Deploy from the repository: deploy/build-site.sh, then deploy/deploy.sh (as the user
#      `deploy`, who logs in with the same SSH key as root); see deploy/README.md.
set -euo pipefail

# Let's Encrypt sends expiry warnings here (renewal itself is automatic). Leave empty to register
# without an e-mail address.
LETSENCRYPT_EMAIL=""

PRIMARY="graphersal.dev"
PLAY="play.graphersal.dev"
REDIRECTS="www.graphersal.dev graphersal.com www.graphersal.com graphersal.org www.graphersal.org"
WEB_ROOT="/var/www/graphersal"
DEPLOY_USER="deploy"

export DEBIAN_FRONTEND=noninteractive

# --- packages and automatic security updates --------------------------------------------------
apt-get update
apt-get -y upgrade
apt-get -y install nginx certbot python3-certbot-nginx ufw unattended-upgrades rsync dnsutils curl
dpkg-reconfigure -f noninteractive unattended-upgrades

# --- firewall: SSH, HTTP, HTTPS only -----------------------------------------------------------
# `limit` allows SSH but refuses an address that opens 6 or more connections within 30 seconds.
ufw limit OpenSSH
ufw allow 'Nginx Full'
ufw --force enable

# --- SSH: keys only, few unauthenticated connections -------------------------------------------
cat > /etc/ssh/sshd_config.d/60-graphersal.conf <<'EOF'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin prohibit-password
EOF
# Every unauthenticated connection is an sshd process of about 15 MB; with the default
# MaxStartups 10:30:100 a burst of them OOM-killed nginx on the 458 MB droplet (2026-11-01).
# At most 4 before random drops, 10 in all, and 20 seconds to log in.
cat > /etc/ssh/sshd_config.d/10-graphersal.conf <<'EOF'
MaxStartups 4:50:10
LoginGraceTime 20
EOF
# sshd -t needs the privilege separation directory, which only a started ssh.service creates.
install -d -m 755 /run/sshd
# Restart only with a valid configuration: a broken one would lock every SSH login out.
if sshd -t; then
  systemctl restart ssh
else
  echo "sshd -t failed: ssh NOT restarted; fix /etc/ssh/sshd_config.d/ and restart it." >&2
fi

# --- swap: 1 GiB, so a burst of memory use slows the droplet instead of killing a service -------
if [ ! -e /swapfile ]; then
  fallocate -l 1G /swapfile
  chmod 600 /swapfile
  mkswap /swapfile
fi
if ! swapon --show=NAME --noheadings | grep -qx /swapfile; then
  swapon /swapfile
fi
if ! grep -q '^/swapfile[[:space:]]' /etc/fstab; then
  echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi

# --- the deploy user: owns the web root, logs in with root's SSH key, no sudo ------------------
if ! id "$DEPLOY_USER" >/dev/null 2>&1; then
  adduser --disabled-password --gecos "" "$DEPLOY_USER"
fi
install -d -m 700 -o "$DEPLOY_USER" -g "$DEPLOY_USER" "/home/$DEPLOY_USER/.ssh"
if [ -f /root/.ssh/authorized_keys ]; then
  install -m 600 -o "$DEPLOY_USER" -g "$DEPLOY_USER" /root/.ssh/authorized_keys \
    "/home/$DEPLOY_USER/.ssh/authorized_keys"
fi

# --- web root with placeholder pages until the first deploy -------------------------------------
install -d -o "$DEPLOY_USER" -g "$DEPLOY_USER" "$WEB_ROOT" "$WEB_ROOT/book" "$WEB_ROOT/play" "$WEB_ROOT/schema"
for site in book play; do
  if [ ! -e "$WEB_ROOT/$site/index.html" ]; then
    printf '<!doctype html><meta charset="utf-8"><title>Graphersal</title><p>Graphersal: coming soon.</p>\n' \
      > "$WEB_ROOT/$site/index.html"
    chown "$DEPLOY_USER:$DEPLOY_USER" "$WEB_ROOT/$site/index.html"
  fi
done

# --- nginx -------------------------------------------------------------------------------------
cat > /etc/nginx/sites-available/graphersal <<EOF
# Caching (deploy/build-site.sh makes it safe):
# - pages and every file without a version in its path: "no-cache", revalidated on each load
#   with the ETag (a 304 when unchanged), so a deploy is visible at the next load;
# - the playground's /v/<version>/ (named by a hash of its content): a year, "immutable";
#   a new release is a new directory, so an old file is never mixed with a new page;
# - the fonts of the book: a week.
# The book, and the URL every Graphersal schema names in "\$schema".
server {
    listen 80;
    listen [::]:80;
    server_name $PRIMARY;
    root $WEB_ROOT/book;

    location = /schema/v1 {
        alias $WEB_ROOT/schema/graphersal-schema-v1.json;
        default_type application/schema+json;
        add_header Access-Control-Allow-Origin "*";
        add_header Cache-Control "public, max-age=86400";
    }
    location ~* \.(woff2?|ttf|otf|eot)\$ {
        add_header Cache-Control "public, max-age=604800";
    }
    location / {
        add_header Cache-Control "no-cache";
        try_files \$uri \$uri/ =404;
    }
    error_page 404 /404.html;
}

# The WebAssembly playground: static files only (.wasm is application/wasm in mime.types).
server {
    listen 80;
    listen [::]:80;
    server_name $PLAY;
    root $WEB_ROOT/play;

    location /v/ {
        add_header Cache-Control "public, max-age=31536000, immutable";
        try_files \$uri =404;
    }
    location / {
        add_header Cache-Control "no-cache";
        try_files \$uri \$uri/ =404;
    }
}

# Every other name: one permanent redirect to the book.
server {
    listen 80;
    listen [::]:80;
    server_name $REDIRECTS;
    return 301 https://$PRIMARY\$request_uri;
}
EOF
ln -sf /etc/nginx/sites-available/graphersal /etc/nginx/sites-enabled/graphersal
rm -f /etc/nginx/sites-enabled/default
# compress text; .wasm too (nginx leaves it out by default)
cat > /etc/nginx/conf.d/graphersal-gzip.conf <<'EOF'
gzip_vary on;
gzip_types text/css application/javascript application/json application/schema+json application/wasm image/svg+xml;
EOF
# nginx restarts after a crash or an OOM kill, and the kernel picks it last when memory runs out.
install -d /etc/systemd/system/nginx.service.d
cat > /etc/systemd/system/nginx.service.d/override.conf <<'EOF'
[Service]
Restart=on-failure
RestartSec=5s
OOMScoreAdjust=-500
EOF
systemctl daemon-reload
nginx -t
systemctl enable nginx
systemctl restart nginx

# --- certificates: a helper to run once the DNS points here -------------------------------------
cat > /usr/local/sbin/graphersal-certs <<EOF
#!/bin/bash
# Requests (or expands) the HTTPS certificate for every Graphersal name that resolves to this
# droplet; names that do not resolve here yet are skipped and listed. Safe to run again.
set -euo pipefail
ip4=\$(curl -fsS http://169.254.169.254/metadata/v1/interfaces/public/0/ipv4/address || true)
ip6=\$(curl -fsS http://169.254.169.254/metadata/v1/interfaces/public/0/ipv6/address || true)
ready=(); waiting=()
for name in $PRIMARY $PLAY $REDIRECTS; do
  # A public resolver, not the droplet's: that one may still hold the registrar's parking
  # address for the record's TTL (Let's Encrypt asks its own resolvers, not this cache).
  a=\$(dig +short A "\$name" @1.1.1.1 | tail -n1)
  if [ -n "\$ip4" ] && [ "\$a" = "\$ip4" ]; then ready+=("-d" "\$name"); else waiting+=("\$name"); fi
done
if [ \${#ready[@]} -eq 0 ]; then
  echo "No name resolves to \$ip4 yet; set the A records and run this again."; exit 1
fi
email_args=(--register-unsafely-without-email)
if [ -n "$LETSENCRYPT_EMAIL" ]; then email_args=(-m "$LETSENCRYPT_EMAIL"); fi
certbot --nginx --non-interactive --agree-tos "\${email_args[@]}" --redirect \\
  --cert-name graphersal --expand "\${ready[@]}"
if [ \${#waiting[@]} -gt 0 ]; then
  echo "Not covered yet (DNS does not point at \$ip4\${ip6:+ / \$ip6}): \${waiting[*]}"
  echo "Run sudo graphersal-certs again when they do."
fi
EOF
chmod 755 /usr/local/sbin/graphersal-certs

# Try once now; it only succeeds for names whose DNS already points at this droplet.
/usr/local/sbin/graphersal-certs || true

echo "Graphersal droplet setup finished."
