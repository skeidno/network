# Windows

The Windows desktop app is the Rust implementation:

- application: `../../rust/`
- WebGUI assets (shared with the Linux headless build): `../../src/network_manager/web/`
- installer entry: `NetworkManager.iss`

Install Inno Setup 7, then run `../../scripts/build_windows_installer.ps1` to build the
application and a per-user installer with Start menu, desktop shortcut, upgrade, and
uninstall support. Pass `-SkipBuild` to package an already built
`dist-rs/NetworkManager` directory instead of running cargo again.

`mihomo.exe` must sit beside `NetworkManager.exe` in `dist-rs/NetworkManager`. The
script copies it from `../../vendor/mihomo.exe` when it is missing there.

The Python desktop GUI that used to live in `src/network_manager/ui/` (PySide6, packaged
with PyInstaller) has been removed — the Rust exe replaces it.
