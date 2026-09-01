# Platform layout

Network Manager is organized as a multi-platform repository.

| Platform | Source | Status |
| --- | --- | --- |
| Windows | Repository root (`src/`, `scripts/`, `tests/`) and `apps/windows/` | Stable |
| Android | `apps/android/` | Native beta; VPN and device tests implemented |
| Linux | Shared Python core and `apps/linux/` | Headless beta; systemd and WebGUI implemented |
| macOS | `apps/macos/` | Planned |
| iOS | `apps/ios/` | Planned |

The Windows project remains at the repository root for existing build and upgrade
compatibility. New platform-specific code must stay below its `apps/<platform>`
directory. Shared schemas will move to a dedicated module only after two platform
implementations use the same contract.

Windows, Linux, and Android currently share the versioned `network-manager-config` JSON
contract documented in `docs/portable-config-v1.md`. SSH profiles, credentials,
process rules, and platform-local ports intentionally remain device-local.
