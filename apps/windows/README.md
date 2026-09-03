# Windows

The production Windows implementation currently lives at the repository root:

- application: `../../src/network_manager/`
- tests: `../../tests/`
- build scripts: `../../scripts/`
- PyInstaller entry: `../../NetworkManager.spec`
- Inno Setup entry: `NetworkManager.iss`
- Desktop shortcut helper: `../../scripts/create_desktop_shortcut.ps1`

`build_windows.ps1` creates a desktop shortcut by default. Pass
`-SkipDesktopShortcut` when producing a clean CI artifact.

Install Inno Setup 7, then run `../../scripts/build_windows_installer.ps1` to build the application and a
per-user installer with Start menu, desktop shortcut, upgrade, and uninstall
support. Pass `-SkipWindowsBuild` to package an existing `dist/NetworkManager`
directory.

This compatibility layout keeps existing build and release commands working.
Windows-only code must not be imported by Android, macOS, or iOS projects.
