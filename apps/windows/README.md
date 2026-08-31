# Windows

The production Windows implementation currently lives at the repository root:

- application: `../../src/network_manager/`
- tests: `../../tests/`
- build scripts: `../../scripts/`
- PyInstaller entry: `../../NetworkManager.spec`
- Desktop shortcut helper: `../../scripts/create_desktop_shortcut.ps1`

`build_windows.ps1` creates a desktop shortcut by default. Pass
`-SkipDesktopShortcut` when producing a clean CI artifact.

This compatibility layout keeps existing build and release commands working.
Windows-only code must not be imported by Android, macOS, or iOS projects.
