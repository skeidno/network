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

## Checking a private CA setup

```
# verify the chain against the CA, without trusting the system store
openssl s_client -connect 127.0.0.1:443 -servername service.example.com \
  -CAfile ca.crt -verify_return_error </dev/null
```

If this reports `Verify return code: 0 (ok)`, a browser will accept the
certificate once the CA is installed as described above.

## Limitations

- A browser or operating system configured with DNS-over-HTTPS or "secure DNS"
  resolves names through its own provider and bypasses the map.
- The map is stored per device and is excluded from the portable configuration,
  because the addresses it contains are only meaningful on one network.
