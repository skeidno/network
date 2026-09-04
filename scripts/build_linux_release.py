from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
LINUX_ROOT = ROOT / "apps" / "linux"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def project_version() -> str:
    for line in (ROOT / "pyproject.toml").read_text(encoding="utf-8").splitlines():
        if line.startswith("version = "):
            return line.split('"', 2)[1]
    raise RuntimeError("Project version not found")


def verified_core(asset: dict[str, str], core_dir: Path, version: str) -> Path:
    core_dir.mkdir(parents=True, exist_ok=True)
    path = core_dir / asset["name"]
    if not path.is_file() or sha256(path) != asset["sha256"]:
        url = f"https://github.com/MetaCubeX/mihomo/releases/download/{version}/{asset['name']}"
        print(f"Downloading {asset['name']}...")
        urllib.request.urlretrieve(url, path)
    actual = sha256(path)
    if actual != asset["sha256"]:
        raise RuntimeError(f"SHA-256 mismatch for {asset['name']}: {actual}")
    return path


def build_wheel(output_dir: Path) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [sys.executable, "-m", "build", "--wheel", "--outdir", str(output_dir), str(ROOT)],
        check=True,
    )
    wheels = list(output_dir.glob("network_manager-*.whl"))
    if len(wheels) != 1:
        raise RuntimeError(f"Expected one wheel, found {len(wheels)}")
    return wheels[0]


def tar_filter(info: tarfile.TarInfo) -> tarfile.TarInfo:
    info.uid = 0
    info.gid = 0
    info.uname = "root"
    info.gname = "root"
    info.mode = 0o755 if info.name.endswith("/install.sh") or info.isdir() else 0o644
    return info


def copy_release_text(source: Path, destination: Path) -> None:
    """Write release text with Unix line endings, even when built on Windows."""
    destination.write_text(source.read_text(encoding="utf-8"), encoding="utf-8", newline="\n")


def main() -> int:
    parser = argparse.ArgumentParser(description="Build ready-to-install Linux release archives")
    parser.add_argument("--output", type=Path, default=ROOT / "release")
    parser.add_argument(
        "--core-dir", type=Path, default=Path(tempfile.gettempdir()), help="Mihomo cache"
    )
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)

    core_manifest = json.loads((LINUX_ROOT / "mihomo.version.json").read_text(encoding="utf-8"))
    app_version = project_version()
    with tempfile.TemporaryDirectory(prefix="network-manager-linux-build-") as temp_value:
        temp_root = Path(temp_value)
        wheel = build_wheel(temp_root / "wheel")
        archives: list[Path] = []
        for arch, asset in core_manifest["assets"].items():
            core = verified_core(asset, args.core_dir, core_manifest["version"])
            package_name = f"NetworkManager-Linux-{arch}-v{app_version}"
            package_root = temp_root / package_name
            package_root.mkdir()
            for source in (
                LINUX_ROOT / "install.sh",
                LINUX_ROOT / "network-manager.service",
                LINUX_ROOT / "README.md",
            ):
                copy_release_text(source, package_root / source.name)
            shutil.copy2(wheel, package_root / wheel.name)
            shutil.copy2(core, package_root / core.name)
            release_manifest = {
                "applicationVersion": app_version,
                "architecture": arch,
                "mihomoVersion": core_manifest["version"],
                "files": {
                    wheel.name: sha256(package_root / wheel.name),
                    core.name: sha256(package_root / core.name),
                },
            }
            (package_root / "manifest.json").write_text(
                json.dumps(release_manifest, ensure_ascii=False, indent=2) + "\n",
                encoding="utf-8",
            )
            checksums = "\n".join(
                f"{value}  {name}" for name, value in release_manifest["files"].items()
            )
            (package_root / "SHA256SUMS").write_text(checksums + "\n", encoding="ascii")

            archive = args.output / f"{package_name}.tar.gz"
            with tarfile.open(archive, "w:gz") as handle:
                handle.add(package_root, arcname=package_name, filter=tar_filter)
            archives.append(archive)
            print(f"Built {archive} ({archive.stat().st_size:,} bytes)")

    (args.output / "SHA256SUMS-Linux.txt").write_text(
        "\n".join(f"{sha256(path)}  {path.name}" for path in archives) + "\n",
        encoding="ascii",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
