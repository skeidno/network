# Platform layout

Network Manager is organized as a multi-platform repository.

| Platform | Source | Status |
| --- | --- | --- |
| Windows | `rust/` (native window + tray) and `apps/windows/` | Stable |
| Android | `apps/android/` | Native beta; VPN and device tests implemented |
| Linux | `rust/` (headless service) and `apps/linux/` | Headless beta; systemd and WebGUI implemented |
| macOS | `apps/macos/` | Planned |
| iOS | `apps/ios/` | Planned |

Both shipped platforms are built from the same Rust crate. The native shell
(tao + wry) is compiled for Windows only; Linux builds the same crate as a
dependency-free service that exposes the identical HTTP/WebGUI API. The shared
frontend lives in `src/network_manager/web/` and is embedded into the binary at
build time. Python remains only as a few build/verification helpers under
`scripts/`.

The Windows project remains at the repository root for existing build and upgrade
compatibility. New platform-specific code must stay below its `apps/<platform>`
directory. Shared schemas will move to a dedicated module only after two platform
implementations use the same contract.

Windows, Linux, and Android currently share the versioned `network-manager-config` JSON
contract documented in `docs/portable-config-v1.md`. SSH profiles, credentials,
process rules, and platform-local ports intentionally remain device-local.
