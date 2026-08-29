from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import urllib.request
import zipfile


PROJECT_ROOT = Path(__file__).resolve().parents[1]
VENDOR_DIR = PROJECT_ROOT / "vendor"
METADATA_PATH = VENDOR_DIR / "mihomo.version.json"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description="Download the pinned official Mihomo build")
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    metadata = json.loads(METADATA_PATH.read_text(encoding="utf-8-sig"))
    version = str(metadata["version"])
    asset = str(metadata["asset"])
    expected = str(metadata["sha256"]).lower()
    executable = VENDOR_DIR / "mihomo.exe"
    if executable.is_file() and not args.force:
        print(f"Mihomo is already available: {executable}")
        return 0

    VENDOR_DIR.mkdir(parents=True, exist_ok=True)
    archive = VENDOR_DIR / (asset + ".tmp")
    url = f"https://github.com/MetaCubeX/mihomo/releases/download/{version}/{asset}"
    request = urllib.request.Request(url, headers={"User-Agent": "NetworkManager-Build"})
    print(f"Downloading {url}")
    try:
        with urllib.request.urlopen(request, timeout=60) as response, archive.open("wb") as output:
            shutil.copyfileobj(response, output)
        actual = sha256(archive)
        if actual != expected:
            raise RuntimeError(f"Mihomo checksum mismatch: expected {expected}, got {actual}")
        with zipfile.ZipFile(archive) as package:
            members = [name for name in package.namelist() if name.lower().endswith(".exe")]
            if len(members) != 1:
                raise RuntimeError("Mihomo package did not contain exactly one executable")
            temporary_executable = executable.with_suffix(".tmp")
            with package.open(members[0]) as source, temporary_executable.open("wb") as output:
                shutil.copyfileobj(source, output)
            temporary_executable.replace(executable)
    finally:
        archive.unlink(missing_ok=True)
    print(f"Installed {version}: {executable}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
