# Local DNS mapping and TLS certificates

Local DNS mapping points a domain at a fixed IP. The storage format, the exact
vs. wildcard matching rules and the `domain IP` text format are described in
[`portable-config-v1.md`](portable-config-v1.md).

The feature is meant for reaching a service on your own network — a NAS, a
router panel, a development server — by name instead of by IP. The map only
changes where the name resolves; it does not change the TLS certificate the
service presents. A browser opened against `https://service.example.com` still
validates that certificate against the device trust store, so two things have to
line up for HTTPS to load without a warning.

## 1. Present a certificate the device trusts

**Public certificate (recommended).** Point a real domain at the service and
issue a Let's Encrypt certificate for it through a DNS-01 challenge (HTTP-01
cannot pass while the name resolves to a private address). Every device already
trusts it and nothing has to be installed.

**Private CA.** A certificate signed by your own CA is accepted only when the CA
is installed as a **system** trust anchor. Android apps that target API 24+
ignore the end-user CA store by default, and Chrome does not use it either, so
importing the CA through the normal "install a certificate" flow is not enough.

- Android: copy the CA to `/system/etc/security/cacerts/<hash>.0`, where `<hash>`
  is `openssl x509 -subject_hash_old -in ca.crt` and the file mode is `0644`.
  Writing to that directory needs a writable system partition — on an emulator
  that means `adb root` plus `adb remount` on an instance started with
  `-writable-system`.
- Windows: `certutil -user -f -addstore Root ca.crt` installs the CA for the
  current user, which does not require administrator rights and is enough for
  Edge and Chrome. Use the machine store instead if every account needs it.

## 2. Keep the leaf certificate short-lived

Chrome rejects a TLS server certificate whose validity period is longer than
**398 days**, and the limit applies to certificates issued by a private,
locally-trusted root as well: a longer leaf fails with
`NET::ERR_CERT_VALIDITY_TOO_LONG` even though the CA itself is trusted. Issue the
leaf with `-days 390` or less. The lifetime of the root CA is not limited.

The certificate also has to cover every mapped name. A wildcard `*.example.com`
covers the apex `example.com` and all of its subdomains, but a certificate that
only carries `*.example.com` still fails for `example.com` itself unless the
apex is listed too. If a mapped name is opened by IP, add an `IP:...` entry to
the subjectAltName.

## 3. Built-in certificate generation

Both apps can do all of the above themselves. Once at least one local DNS
mapping exists, the app issues a private CA plus a leaf certificate whose
subjectAltName covers exactly the mapped names — a wildcard entry also adds the
apex, and a literal IP becomes an `IP:` entry. The leaf is valid for **397
days** (under Chrome's 398-day ceiling) and is re-signed automatically when the
mapping set changes, when the files are missing, or within 30 days of expiry.
The CA is created once and never rotated, so trust survives leaf renewals.

Files live in `local-certs/` under the app data directory (`ca.crt`, `ca.key`,
`server.crt`, `server.key`, `meta.json`). The desktop app exposes them on the
settings page — status lines for the CA, the leaf, the trust state and the
directory, plus buttons to regenerate, install (`certutil -user -f -addstore
Root`), uninstall, and open the folder. `saveLocalDns` regenerates
transparently, so a new mapping is usable over HTTPS right after it is saved.

The Android app shows the same status on a "本地证书" card in the toolbox and
offers three trust paths:

- **安装 CA** opens the system certificate installer (KeyChain); needs a
  lockscreen credential. User-installed CAs are not trusted by apps targeting
  API 24+ by default, but Chrome does honor them.
- **Root 安装到系统库** writes the CA to `/system/etc/security/cacerts/<hash>.0`
  via `su`. Both `su -c` (Magisk) and `su 0 sh -c` (AOSP/toybox) syntaxes are
  attempted, because some su implementations reject `-c`.
- **导出 ca.crt** copies the CA to Downloads for manual distribution.

Trust-state detection on Windows queries `certutil -user -store Root` with a
short cache, so the status line reflects the store right after install or
uninstall.

The issued leaf also carries an **Authority Key Identifier** and claims only
the key usages an EC key may claim (`digitalSignature`; RFC 5480 forbids
`keyEncipherment` on EC certificates). Lenient clients accept certificates
without these, but strict ones do not: Python 3.13 enables
`VERIFY_X509_STRICT` by default and OpenSSL fails with
`Missing Authority Key Identifier`. Existing leaves issued by an older build
are re-signed once automatically (tracked by a schema marker in `meta.json`).

## Serving a mapped domain from your own code

`scripts/local_https_flask.py` is a self-contained Flask example that serves
HTTPS on the mapped names with the certificate setup described above:

```
pip install flask cryptography
python scripts/local_https_flask.py             # 0.0.0.0:443
python scripts/local_https_flask.py --check     # mappings, certs, trust state
python scripts/local_https_flask.py --certs-only
python scripts/local_https_flask.py --install-ca
```

It reads the same `settings.json`, reuses the app-generated CA (so the root you
already trusted keeps working) and only signs its own leaf — into
`local-certs/flask-https/`, never overwriting the app's `server.crt` or
`meta.json` — when the app-issued one does not cover the mapped names or is
close to expiry. The leaf additionally covers `localhost`, `127.0.0.1` and
`::1`, so the chain can be tested without a mapping. The served page reports
the request host, the certificate names and the mapping table; `/healthz` and
`/echo` return JSON for scripted checks.

Notes for other clients:

- **curl on Windows** uses Schannel, which refuses to proceed when the
  revocation status of a private CA cannot be checked. Add `--ssl-no-revoke`
  (or use a curl built against OpenSSL).
- **Python 3.13** (`requests`, `httpx`, `urllib3`) verifies strictly by
  default; the certificates issued by the app and by the script satisfy it.

## Checking a private CA setup

```
# verify the chain against the CA, without trusting the system store
openssl s_client -connect 127.0.0.1:443 -servername service.example.com \
  -CAfile ca.crt -verify_return_error </dev/null

# the strict check the Python toolchain effectively performs
openssl verify -x509_strict -CAfile ca.crt server.crt
```

If this reports `Verify return code: 0 (ok)`, a browser will accept the
certificate once the CA is installed as described above.

## Limitations

- A browser or operating system configured with DNS-over-HTTPS or "secure DNS"
  resolves names through its own provider and bypasses the map.
- The map is stored per device and is excluded from the portable configuration,
  because the addresses it contains are only meaningful on one network.

## DNS answer mode: fake-ip vs redir-host

The generated core config answers DNS in one of two modes (Settings → Local DNS →
"DNS compatibility mode"):

- **fake-ip** (default). Every domain outside the built-in filter list answers with
  a synthetic `198.18.x.x` address; the core recovers the real domain when the
  connection arrives and matches rules by name. Fastest, and what most setups want.
  The filter list always answers with real IPs for names where a synthetic address
  breaks things: localhost/LAN suffixes, NTP servers, OS connectivity probes
  (`msftconnecttest.com`, `captive.apple.com`, …) and captcha endpoints that
  validate the peer address range.
- **redir-host** ("DNS compatibility mode"). Every domain answers with its real IP
  and name matching falls back to the connection sniffer. This is the escape hatch
  for apps that cache, report, or dial the synthetic address directly — the
  "works with TUN off, breaks with TUN on" class of failures. Cost: one real
  resolution per connection.

In both modes the resolvers follow the routing rules (`respect-rules`): domains the
rules send direct are resolved by domestic DoH servers (`direct-nameserver`), and
proxy server hostnames are always resolved domestically (`proxy-server-nameserver`)
so resolving a node never depends on being connected to it.

Changing the mode takes effect the next time the core (re)starts — toggle the VPN
off and on again after saving.
