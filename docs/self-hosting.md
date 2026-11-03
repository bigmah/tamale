# Hosting your own tamale-hub

The hub is a relay and uses very little CPU or memory, so it can share a small server with other
sites. Each session gets its own origin at `<sid>.hub.example`, which is why it needs a wildcard
DNS record and a wildcard certificate. `hub.example` stands for your domain throughout.

You need:

- a server with a public IP, with ports 80 and 443 open, and Rust to build the hub;
- a domain whose DNS has an API [Caddy has a plugin for](https://github.com/caddy-dns). Let's
  Encrypt issues wildcard certificates only through a DNS challenge. Route 53 is shown here;
- Caddy, which terminates TLS in front of the hub.

## Try it locally first

No DNS or TLS needed, because browsers resolve `*.localhost` to this machine:

```sh
cargo build -p tamale-hub --release --locked
H=./target/release/tamale-hub
$H key new mine --public-url http://hub.localhost:8787 --data-dir /tmp/hub
$H serve --insecure --public-url http://hub.localhost:8787 --data-dir /tmp/hub
```

Then, in another terminal, run tamale and `/remote`:

```sh
TAMALE_HUB_URL=ws://hub.localhost:8787 TAMALE_HUB_KEY=thk_… tamale
```

## 1. DNS

Add `A` records for the domain and the wildcard. Add `AAAA` records too only if the server has
IPv6.

| Name | Type | Value |
| --- | --- | --- |
| `hub.example` | A | your server's IP |
| `*.hub.example` | A | your server's IP |

Check with `dig +short hub.example` and `dig +short anything.hub.example`.

## 2. A DNS credential for Caddy

Give Caddy a credential that can only change the `_acme-challenge` TXT record. On Route 53, make
an IAM user with no console access, an access key and this inline policy:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": ["route53:ListHostedZones", "route53:ListHostedZonesByName"],
      "Resource": "*"
    },
    {
      "Effect": "Allow",
      "Action": ["route53:GetChange"],
      "Resource": "arn:aws:route53:::change/*"
    },
    {
      "Effect": "Allow",
      "Action": ["route53:ListResourceRecordSets"],
      "Resource": "arn:aws:route53:::hostedzone/YOUR_ZONE_ID"
    },
    {
      "Effect": "Allow",
      "Action": ["route53:ChangeResourceRecordSets"],
      "Resource": "arn:aws:route53:::hostedzone/YOUR_ZONE_ID",
      "Condition": {
        "ForAllValues:StringEquals": {
          "route53:ChangeResourceRecordSetsNormalizedRecordNames": ["_acme-challenge.hub.example"],
          "route53:ChangeResourceRecordSetsRecordTypes": ["TXT"]
        }
      }
    }
  ]
}
```

If issuance fails with an access error, compare this with the `caddy-dns/route53` README, as the
plugin may need one more read action.

**Keep the key out of the environment.** Caddy's stock unit runs `caddy run --environ`, which
prints every environment variable to the journal at startup. Use the credentials file in the
`caddy` user's home instead:

```sh
sudo install -d -m 700 -o caddy -g caddy /var/lib/caddy/.aws
sudo install -m 600 -o caddy -g caddy /dev/null /var/lib/caddy/.aws/credentials
sudo -e /var/lib/caddy/.aws/credentials
```

```ini
[default]
aws_access_key_id = …
aws_secret_access_key = …
region = us-east-1
```

## 3. Caddy with the DNS plugin

Build Caddy with [`xcaddy`](https://github.com/caddyserver/xcaddy), using the Go version that
Caddy release was built with. A mismatch can panic when Caddy loads its config, so validate
before switching:

```sh
xcaddy build --with github.com/caddy-dns/route53 --output ./caddy-custom
./caddy-custom validate --config /etc/caddy/Caddyfile --adapter caddyfile
```

**If Caddy came from apt, divert the stock binary.** Otherwise a later `apt upgrade` replaces your
build with one that lacks the plugin, and every site on the server goes down at the next restart:

```sh
sudo dpkg-divert --divert /usr/bin/caddy.default --rename /usr/bin/caddy
sudo install -m 755 ./caddy-custom /usr/bin/caddy.custom
sudo update-alternatives --install /usr/bin/caddy caddy /usr/bin/caddy.default 10
sudo update-alternatives --install /usr/bin/caddy caddy /usr/bin/caddy.custom 50
sudo systemctl restart caddy
```

To undo this, run `sudo update-alternatives --set caddy /usr/bin/caddy.default`, remove the hub's
block from the Caddyfile and restart Caddy.

Add this block to `/etc/caddy/Caddyfile`, leaving any other sites' blocks alone. The wildcard
doesn't cover `hub.example` itself, so it lists both names:

```
hub.example, *.hub.example {
    tls {
        dns route53
    }
    reverse_proxy 127.0.0.1:8787
}
```

Apply it:

```sh
caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile && sudo systemctl reload caddy
```

`curl -I https://hub.example` and `curl -I https://x.hub.example` should now both get a valid
certificate. A 502 is expected until the hub runs. If issuance stalls on DNS propagation, add
`resolvers 1.1.1.1` inside the `tls` block. Logs are in `journalctl -u caddy -f`.

## 4. The hub as a service

Build the hub and make a user for it. On a small server, add `-j 1` to the build so it doesn't
starve whatever else runs there:

```sh
git clone https://github.com/bigmah/tamale.git && cd tamale
cargo build -p tamale-hub --release --locked
sudo install -m 755 target/release/tamale-hub /usr/local/bin/
sudo useradd --system --home /var/lib/tamale-hub --create-home tamale-hub
```

Write `/etc/systemd/system/tamale-hub.service`:

```ini
[Unit]
Description=tamale-hub
After=network-online.target
Wants=network-online.target

[Service]
User=tamale-hub
ExecStart=/usr/local/bin/tamale-hub serve --listen 127.0.0.1:8787 --public-url https://hub.example --data-dir /var/lib/tamale-hub
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/tamale-hub
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

Then start it with `sudo systemctl enable --now tamale-hub`.

## 5. Keys

Make a key and point each tamale at your hub:

```sh
sudo -u tamale-hub tamale-hub key new mine --data-dir /var/lib/tamale-hub --public-url https://hub.example
```

```sh
export TAMALE_HUB_URL=wss://hub.example  # or set [hub] url
export TAMALE_HUB_KEY=thk_…
```

The key is printed once, with a QR code for your phone. See [Remote access](remote-access.md) for
the rest.

```sh
tamale-hub key new <name>       # prints the key once, with a QR code
tamale-hub key list
tamale-hub key revoke <name>    # drops the key's sessions and tabs
```

Run these as the service user with the same `--data-dir`, so the files keep their owner. A running
hub picks up changes within two seconds (or on `systemctl reload tamale-hub`). Keys made this way
never expire.

**Letting visitors make keys.** By default only the CLI makes keys. Add `--signup open` to `serve`
and anyone can make one on the hub's page, limited to 5 new keys per IP per day and `--max-keys`
(default 500) in all. Page-made keys that never connect are deleted after 7 days, and after 90 days
unused.

**The iPhone app.** The app (see [`ios.md`](ios.md)) works with any hub: it takes the hub from the
key's QR code. Hubs from before it sign the app in with cookies, as a browser; newer ones take its
key as `Authorization: Bearer`. For the terminal's `/open/<sid>` codes to open the app rather than
Safari, start `serve` with `--apple-app-id <team id>.<bundle id>` (or set
`TAMALE_HUB_APPLE_APP_ID`), which serves `/.well-known/apple-app-site-association`, and add your
hub's domain to the app's associated domains.

**What the hub stores.** The data dir holds `state.json` (mode 600): SHA-256 hashes of the keys'
auth values and the secret that signs session cookies. No keys, no sessions, no history.

## Upgrading

Upgrade the hub and tamale together: a tamale from before keys gets a 401 from a hub with them, and
the other way round.

`deploy.sh` deploys from `main` on a schedule, for root's cron on the server. It builds only new
commits, restarts the hub only if its binary changed, and puts the old binary back if the new one
doesn't come up:

```
*/5 * * * * /root/tamale/deploy.sh 2>&1 | logger -t tamale-deploy
```

To use it on your own hub, change `tamale-hub.com` to your domain on the `DEPLOY_HOST` line. It
also builds with the `1.93.0` toolchain, which needs installing with
`rustup toolchain install 1.93.0`.

To upgrade a custom Caddy, rebuild it for the new version, `validate` the Caddyfile with it, replace
`/usr/bin/caddy.custom` and restart. `apt upgrade` now only updates `/usr/bin/caddy.default`.
