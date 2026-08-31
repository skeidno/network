# Portable configuration v1

`network-manager-config` is the cross-device JSON format used by Windows,
Android, macOS, and iOS clients. Version 1 carries routing mode, fallback,
portable domain/IP rules, subscriptions, selected node, and Clash-compatible
node objects.

The format intentionally excludes SSH server profiles, SSH credentials,
desktop process rules, local listening ports, controller secrets, and
platform startup preferences. Importing a portable file preserves those local
settings on the receiving desktop.

`routing.mode` accepts `rule`, `global`, `smart`, or `direct`. `smart` is
portable across desktop and mobile clients and uses periodic health checks to
select a low-latency node without carrying any SSH state.

Node objects may contain proxy passwords and private identifiers. Treat an
exported file as sensitive and only move it through trusted storage.
