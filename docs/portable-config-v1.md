# Portable configuration v1

`network-manager-config` is the cross-device JSON format used by Windows,
Android, macOS, and iOS clients. Version 1 carries routing mode, fallback,
portable domain/IP rules, subscriptions, selected node, and Clash-compatible
node objects.

The format intentionally excludes SSH server profiles, SSH credentials,
desktop process rules, local listening ports, controller secrets, and
platform startup preferences. Importing a portable file preserves those local
settings on the receiving desktop.

The local DNS map (custom domain to fixed IP) is excluded for the same reason:
the addresses it points at are only meaningful on one network. Desktop keeps it
in `local_dns` inside `settings.json`, Android keeps it in its own preferences,
and both ends share the same text format — one `domain IP` pair per line, field
order free, `#` starts a comment.

`routing.mode` accepts `rule`, `global`, `smart`, or `direct`. `smart` is
portable across desktop and mobile clients and uses periodic health checks to
select a low-latency node without carrying any SSH state.

Node objects may contain proxy passwords and private identifiers. Treat an
exported file as sensitive and only move it through trusted storage.
