# -*- mode: python ; coding: utf-8 -*-


a = Analysis(
    ['src\\network_manager\\__main__.py'],
    pathex=['src'],
    binaries=[('vendor\\mihomo.exe', 'vendor')],
    datas=[('src\\network_manager\\style.qss', 'network_manager'), ('src\\network_manager\\web', 'network_manager\\web'), ('THIRD_PARTY_NOTICES.md', '.')],
    hiddenimports=[],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
    optimize=0,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name='NetworkManager',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    uac_admin=True,
    version='NetworkManager.version.txt',
    icon=['src\\network_manager\\web\\icons\\network-manager.ico'],
)
coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=True,
    upx_exclude=[],
    name='NetworkManager',
)
