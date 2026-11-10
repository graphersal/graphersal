# Deploying the static sites

The book and the WebAssembly playground are static files, served by nginx on one small
DigitalOcean droplet:

| Address | Content |
|---|---|
| `https://graphersal.dev/` | the book (`mdbook build book`) |
| `https://graphersal.dev/schema/v1` | the schema meta-schema (`crates/graphersal/src/graph/schema/graphersal-schema-v1.json`), the URL every schema names in `"$schema"` |
| `https://play.graphersal.dev/` | the playground (`playground/` with its `pkg/`) |
| `https://memory.graphersal.dev/` | the graphersal-memory site: one static page (`index.html` + `assets/`), its source kept outside this repository; served with `noindex` while it is a draft |
| `www.*`, `graphersal.com`, `graphersal.org` | a permanent redirect to `https://graphersal.dev/` |

## The droplet (once)

1. Create an Ubuntu 24.04 LTS droplet with an SSH key and IPv6, and paste
   [`droplet-setup.sh`](droplet-setup.sh) into **Startup Scripts** (set `LETSENCRYPT_EMAIL` in it
   first if you want expiry warnings). It installs nginx, certbot, the firewall (SSH, HTTP,
   HTTPS), automatic security updates, a `deploy` user that owns `/var/www/graphersal` and logs in
   with the same key, and the nginx sites. Its log: `/var/log/cloud-init-output.log`.

   It also hardens the droplet against running out of memory: sshd accepts few unauthenticated
   connections (`MaxStartups 4:50:10`, `LoginGraceTime 20` in
   `/etc/ssh/sshd_config.d/10-graphersal.conf`), ufw rate-limits SSH (`ufw limit OpenSSH`), a
   1 GiB swap file (`/swapfile`), and nginx restarts by itself and is the kernel's last OOM choice
   (`Restart=on-failure`, `OOMScoreAdjust=-500` in
   `/etc/systemd/system/nginx.service.d/override.conf`). The reason: on 2026-11-01 at 05:34 UTC a
   burst of unauthenticated SSH connections (about 15 MB per pre-auth sshd under the default
   `MaxStartups 10:30:100`) exhausted the 458 MB droplet without swap, and the kernel OOM-killed
   nginx.

   On an existing droplet, set up before this, as root:

   ```bash
   printf 'MaxStartups 4:50:10\nLoginGraceTime 20\n' > /etc/ssh/sshd_config.d/10-graphersal.conf
   sshd -t && systemctl restart ssh
   ufw limit OpenSSH
   [ -e /swapfile ] || { fallocate -l 1G /swapfile && chmod 600 /swapfile && mkswap /swapfile; }
   swapon --show=NAME --noheadings | grep -qx /swapfile || swapon /swapfile
   grep -q '^/swapfile[[:space:]]' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
   mkdir -p /etc/systemd/system/nginx.service.d
   printf '[Service]\nRestart=on-failure\nRestartSec=5s\nOOMScoreAdjust=-500\n' \
     > /etc/systemd/system/nginx.service.d/override.conf
   systemctl daemon-reload && systemctl restart nginx
   ```
2. DNS (the registrar's DNS, which keeps its e-mail forwarding): A and AAAA records for `@`,
   `www`, `play` and `memory` of `graphersal.dev`, and for `@` and `www` of `graphersal.com` and
   `graphersal.org`, all pointing at the droplet.
3. When the names resolve: `ssh root@<droplet> graphersal-certs`. It requests the HTTPS
   certificate for every name that points at the droplet and lists the others; run it again when
   they resolve. certbot renews the certificate by itself.
4. An SSH alias for the deploy script, in `~/.ssh/config`:

   ```text
   Host graphersal-web
       HostName <droplet IP>
       User deploy
   ```

## A deploy

```bash
deploy/build-site.sh       # target/site/: the book, the playground, the schema
deploy/deploy.sh           # uploads it to graphersal-web (or: deploy/deploy.sh <ssh host>)
```

The graphersal-memory site has its own nginx file (`/etc/nginx/sites-available/graphersal-memory`)
and its own certificate (`graphersal-memory`, requested by `graphersal-certs`), so it changes
without touching the other sites. A deploy is one rsync from the folder holding its `index.html`
(the folder's `README.md` and dot files stay local):

```bash
rsync -rlzcO --delete --exclude README.md --exclude '.*' ./ graphersal-web:/var/www/graphersal/memory/
```

Before the product's launch remove the two `X-Robots-Tag` lines from that nginx file (and from
`droplet-setup.sh`), then `sudo nginx -t && sudo systemctl reload nginx`.

## Caching

Browsers keep files long only where a changed file always gets a new URL:

- The playground's files live under `/v/<version>/`, where the version is a hash of their
  content (`build-site.sh`). nginx sends `Cache-Control: public, max-age=31536000, immutable`
  there: a browser loads them once per release. Every path inside is relative (modules, the
  worker and the wasm are resolved through `import.meta.url`), so the tree is copied as it is.
- `index.html` (which names the version) and `reset/` are sent with `no-cache`: the browser asks
  on every load and gets a `304 Not Modified` while nothing changed. A deploy is therefore
  visible at the next load, and an old cached module is never combined with a new page.
- The book (mdBook 0.4 names its files without hashes) is `no-cache` throughout, revalidated by
  ETag; only its fonts are cached for a week. `deploy.sh` uploads by checksum and keeps the server
  time of unchanged files, so their ETags stay and revalidation stays a 304.
- `deploy.sh` uploads the new version before the `index.html` that names it, and keeps the newest
  five versions, because an open tab loads some modules lazily from its own version.
- The libraries from cdnjs and jsDelivr are pinned to exact versions in their URLs (with
  Subresource Integrity), so the browser caches them as the CDNs say.

A local check with the same headers: serve `target/site/play` with any static server that sends
`no-cache` outside `/v/` (the development server `playground/serve.py` sends `no-store` for
everything, which is right while editing).

## The mirror on GitHub Pages

The same sites are mirrored on GitHub Pages, from the repository
`github.com/graphersal/graphersal.github.io`:

| Address | Content |
|---|---|
| `https://graphersal.github.io/` | the book |
| `https://graphersal.github.io/play/` | the playground |

```bash
deploy/build-site.sh && deploy/mirror.sh    # after (or instead of) deploy/deploy.sh
```

`mirror.sh` copies `target/site/`, rewrites the links between the book and the playground to the
mirror's addresses, and pushes it as ONE commit that replaces the previous one (no history of old
playground versions). GitHub Pages serves every file with a 10-minute cache; the playground's
`v/<version>/` layout keeps a page and its modules consistent anyway.
