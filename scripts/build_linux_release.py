from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
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
    """版本号以 rust/Cargo.toml 为准：Python 包已经删掉，Rust 是唯一的产品代码。"""
    in_package = False
    for line in (ROOT / "rust" / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if in_package and stripped.startswith("version = "):
            return stripped.split('"', 2)[1]
    raise RuntimeError("Version not found in rust/Cargo.toml")


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


def build_binary(output_dir: Path) -> Path:
    """编出 Linux 二进制。

    Linux 产物是 Rust 单文件，必须在本机（或 CI 的 ubuntu runner）上用 cargo 编；
    Windows 上交叉编译缺 glibc 目标，编不出来，所以这里只负责调 cargo，不做交叉编译。
    """
    output_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["cargo", "build", "--release", "--manifest-path", str(ROOT / "rust" / "Cargo.toml")],
        check=True,
    )
    built = ROOT / "rust" / "target" / "release" / "network-manager-rs"
    if not built.is_file():
        raise RuntimeError(f"cargo 没有产出 {built}")
    staged = output_dir / "network-manager-rs"
    shutil.copy2(built, staged)
    return staged


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
    parser.add_argument(
        "--binary",
        type=Path,
        default=None,
        help="已经编好的 network-manager-rs（不给就在这里调 cargo build）",
    )
    parser.add_argument(
        "--arch",
        action="append",
        choices=["amd64", "arm64"],
        help="只打指定架构的包（可重复）；默认全部。二进制是架构相关的，"
        "在哪个机器上编的就得只打那个架构。",
    )
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)

    core_manifest = json.loads((LINUX_ROOT / "mihomo.version.json").read_text(encoding="utf-8"))
    app_version = project_version()
    with tempfile.TemporaryDirectory(prefix="network-manager-linux-build-") as temp_value:
        temp_root = Path(temp_value)
        if args.binary is not None:
            binary = temp_root / "network-manager-rs"
            shutil.copy2(args.binary, binary)
        else:
            binary = build_binary(temp_root)
        archives: list[Path] = []
        for arch, asset in core_manifest["assets"].items():
            if args.arch and arch not in args.arch:
                continue
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
            # 同一个二进制打进每个架构包：install.sh 按 uname -m 挑 mihomo，
            # 程序本体则要求包与机器架构匹配（发布时包名已标明 arch）。
            shutil.copy2(binary, package_root / binary.name)
            shutil.copy2(core, package_root / core.name)
            release_manifest = {
                "applicationVersion": app_version,
                "architecture": arch,
                "mihomoVersion": core_manifest["version"],
                "files": {
                    binary.name: sha256(package_root / binary.name),
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
