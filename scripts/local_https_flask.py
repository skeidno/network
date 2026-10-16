#!/usr/bin/env python3
"""本地 DNS + HTTPS 一键验证：Flask 示例服务。

配合程序的「本地 DNS 映射」与「内置证书生成」使用，把三段链路一次跑通：

    DNS 解析对  →  证书名字对  →  证书被信任

它做的事：

* **复用程序已经生成的根 CA**（`<数据目录>/local-certs/ca.crt`）。已经装进信任库的根
  继续有效，脚本不另起炉灶 —— 换根意味着你要重新信任一次。
* **按映射表准备叶证书**。名字不够用/快到期/根换了，就在
  `<数据目录>/local-certs/flask-https/` 自签一张 ≤397 天的叶证书（Chrome Root Store
  对私有根签发的叶证书同样限 398 天，超一天就是 `NET::ERR_CERT_VALIDITY_TOO_LONG`）。
  程序的 `server.crt` / `server.key` / `meta.json` **不会被改写**，程序界面里的状态
  依然属实。
* 起一个 Flask HTTPS 服务。页面能打开，就说明 DNS + 证书 + 信任都通了。

依赖（两个）：

    pip install flask cryptography

用法：

    python local_https_flask.py                    # 准备证书，跑在 0.0.0.0:443
    python local_https_flask.py --port 8443        # 443 被占用时换个口
    python local_https_flask.py --check            # 只看现状：映射、证书、信任状态
    python local_https_flask.py --certs-only       # 只准备证书并打印路径，不起服务
    python local_https_flask.py --install-ca       # Windows：把根证书装进当前用户信任库
    python local_https_flask.py --uninstall-ca     # Windows：取消信任
    python local_https_flask.py --dns-file dns.txt # 用程序导出的映射文本（一行一条）
    python local_https_flask.py --name myapp.test  # 临时追加一个要覆盖的名字
    python local_https_flask.py --http-port 80     # 顺带起 80 端口跳转到 HTTPS

程序用了 `NETWORK_MANAGER_DATA_DIR` 换数据目录时，脚本会自动跟随这个环境变量；
也可以用 `--data-dir` 显式指定。
"""

from __future__ import annotations

import argparse
import ipaddress
import json
import os
import platform
import socket
import subprocess
import sys
import threading
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

try:
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID
except ImportError:  # pragma: no cover - 提示比堆栈有用
    sys.exit("缺少 cryptography：pip install cryptography")
try:
    from flask import Flask, jsonify, request
except ImportError:  # pragma: no cover
    sys.exit("缺少 flask：pip install flask")


# ---------------------------------------------------------------------------
# 与程序侧一致的常量
# ---------------------------------------------------------------------------

APP_DIR_NAME = "NetWorkManger"  # 与 rust/src/paths.rs 的 APP_NAME 保持一致

LEAF_VALIDITY_DAYS = 397  # 上限 398 天（Chrome Root Store），留一天余量
CA_VALIDITY_DAYS = 3650  # 398 天限制只作用于叶证书，根给十年
RENEW_BEFORE_DAYS = 30  # 叶证书剩这么多天就重签

CA_COMMON_NAME = "Network Manager Local CA"
CA_ORGANIZATION = "Network Manager"
LEAF_COMMON_NAME = "Network Manager Local Dev"

CERT_DIR_NAME = "local-certs"
FLASK_CERT_DIR_NAME = "flask-https"  # 脚本自签的叶证书放这里，避开程序的文件

# 叶证书额外带上回环名字，方便直接 `https://localhost/` 自测。
LOOPBACK_NAMES = ("localhost", "127.0.0.1", "::1")


# ---------------------------------------------------------------------------
# 路径与配置
# ---------------------------------------------------------------------------


def data_dir(explicit: str | None) -> Path:
    """程序的数据目录。与 rust/src/paths.rs 的解析顺序一致。"""
    if explicit:
        return Path(explicit).expanduser()
    env = os.environ.get("NETWORK_MANAGER_DATA_DIR")
    if env:
        return Path(env).expanduser()
    system = platform.system()
    if system == "Windows":
        base = Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local")
    elif system == "Darwin":
        base = Path.home() / "Library" / "Application Support"
    else:
        base = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local" / "share")
    return base / APP_DIR_NAME


def load_mappings(cfg: Path) -> list[tuple[str, str]]:
    """`settings.json` 里的 `local_dns` → [(域名, 地址)]。"""
    raw = cfg / "settings.json"
    if not raw.is_file():
        print(f"! 没找到 {raw}，按「没有映射」处理")
        return []
    try:
        payload = json.loads(raw.read_text(encoding="utf-8"))
    except (OSError, ValueError) as err:
        print(f"! 读 {raw} 失败：{err}")
        return []

    entries = payload.get("local_dns")
    if entries is None:  # 兼容旧字段名
        entries = payload.get("localDns") or []
    result: list[tuple[str, str]] = []
    for item in entries:
        if not isinstance(item, dict):
            continue
        domain = str(item.get("domain", "")).strip().lower()
        address = str(item.get("address", "")).strip()
        if domain and address:
            result.append((domain, address))
    return result


def mappings_from_text(path: Path) -> list[tuple[str, str]]:
    """程序导出的映射文本：一行一条 `域名 地址`，`#` 开头是注释。"""
    result: list[tuple[str, str]] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.replace(",", " ").split()
        if len(parts) < 2:
            continue
        result.append((parts[0].lower(), parts[1]))
    return result


def is_ip(text: str) -> bool:
    try:
        ipaddress.ip_address(text.strip().strip("[]"))
        return True
    except ValueError:
        return False


def required_names(
    mappings: list[tuple[str, str]], extra: list[str], loopback: bool = True
) -> list[str]:
    """叶证书必须覆盖的名字。

    规则与程序一致：通配条目补上裸域（`*.baidu.com` 在 X.509 里**不**覆盖
    `baidu.com`），映射里出现字面量 IP 也写进 SAN。

    `loopback=False` 时只算映射推导出来的名字 —— 那才是程序会写进 SAN 的集合，
    用来判断「程序签的证书够不够用」；回环名字只在我们自己签证书时才额外补上。
    """
    names: list[str] = []
    for domain, address in mappings:
        if domain.startswith("*.") and len(domain) > 2:
            suffix = domain[2:].strip(".")
            for name in (f"*.{suffix}", suffix):
                if name not in names:
                    names.append(name)
        elif domain not in names:
            names.append(domain)
        if is_ip(address) and address not in names:
            names.append(address)
    tail = (list(LOOPBACK_NAMES) + list(extra)) if loopback else list(extra)
    for name in tail:
        if name and name not in names:
            names.append(name)
    return sorted(names)


# ---------------------------------------------------------------------------
# 证书
# ---------------------------------------------------------------------------


def _new_serial() -> int:
    return x509.random_serial_number()


def _window(days: int) -> tuple[datetime, datetime]:
    """起点取当天 00:00 UTC（对任何时区都已是过去），有效期正好 `days` 天。"""
    start = datetime.now(timezone.utc).replace(hour=0, minute=0, second=0, microsecond=0)
    return start, start + timedelta(days=days)


def _read_cert(path: Path) -> x509.Certificate | None:
    try:
        return x509.load_pem_x509_certificate(path.read_bytes())
    except (OSError, ValueError):
        return None


def _read_key(path: Path) -> ec.EllipticCurvePrivateKey | None:
    try:
        key = serialization.load_pem_private_key(path.read_bytes(), password=None)
    except (OSError, ValueError):
        return None
    return key if isinstance(key, ec.EllipticCurvePrivateKey) else None


def _write(path: Path, payload: bytes, secret: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(payload)
    if secret and os.name == "posix":
        os.chmod(path, 0o600)


def _pem_cert(cert: x509.Certificate) -> bytes:
    return cert.public_bytes(serialization.Encoding.PEM)


def _pem_key(key: ec.EllipticCurvePrivateKey) -> bytes:
    return key.private_bytes(
        encoding=serialization.Encoding.PEM,
        format=serialization.PrivateFormat.PKCS8,
        encryption_algorithm=serialization.NoEncryption(),
    )


def _name(common_name: str) -> x509.Name:
    return x509.Name(
        [
            x509.NameAttribute(NameOID.COMMON_NAME, common_name),
            x509.NameAttribute(NameOID.ORGANIZATION_NAME, CA_ORGANIZATION),
        ]
    )


def create_ca(path: Path) -> None:
    """自签根证书。只在程序没生成过 CA 时才走到这里。"""
    key = ec.generate_private_key(ec.SECP256R1())
    start, end = _window(CA_VALIDITY_DAYS)
    subject = _name(CA_COMMON_NAME)
    cert = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(subject)
        .public_key(key.public_key())
        .serial_number(_new_serial())
        .not_valid_before(start)
        .not_valid_after(end)
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .add_extension(
            x509.KeyUsage(
                digital_signature=True,
                content_commitment=False,
                key_encipherment=False,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=True,
                crl_sign=True,
                encipher_only=False,
                decipher_only=False,
            ),
            critical=True,
        )
        .add_extension(
            x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False
        )
        .sign(key, hashes.SHA256())
    )
    _write(path / "ca.crt", _pem_cert(cert))
    _write(path / "ca.key", _pem_key(key), secret=True)


def sign_leaf(
    ca_cert: x509.Certificate,
    ca_key: ec.EllipticCurvePrivateKey,
    names: list[str],
    target: Path,
) -> x509.Certificate:
    """按 `names` 签一张叶证书（ECDSA P-256 / SHA256withECDSA，与程序一致）。"""
    key = ec.generate_private_key(ec.SECP256R1())
    leaf = (
        x509.CertificateBuilder()
        .subject_name(_name(LEAF_COMMON_NAME))
        .issuer_name(ca_cert.subject)
        .public_key(key.public_key())
        .serial_number(_new_serial())
        .not_valid_before(_window(LEAF_VALIDITY_DAYS)[0])
        .not_valid_after(_window(LEAF_VALIDITY_DAYS)[1])
        .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
        # EC 密钥只做数字签名（ECDHE_ECDSA），不该声明 keyEncipherment。
        .add_extension(
            x509.KeyUsage(
                digital_signature=True,
                content_commitment=False,
                key_encipherment=False,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=False,
                crl_sign=False,
                encipher_only=False,
                decipher_only=False,
            ),
            critical=True,
        )
        .add_extension(
            x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False
        )
        .add_extension(
            x509.SubjectAlternativeName([_general_name(n) for n in names]), critical=False
        )
        .add_extension(
            x509.AuthorityKeyIdentifier.from_issuer_public_key(ca_cert.public_key()),
            critical=False,
        )
        .add_extension(
            x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False
        )
        .sign(ca_key, hashes.SHA256())
    )
    _write(target / "server.crt", _pem_cert(leaf))
    _write(target / "server.key", _pem_key(key), secret=True)
    return leaf


def _general_name(name: str) -> x509.GeneralName:
    if is_ip(name):
        return x509.IPAddress(ipaddress.ip_address(name.strip().strip("[]")))
    return x509.DNSName(name)


def cert_names(cert: x509.Certificate) -> tuple[set[str], set[str]]:
    """证书里实际写着的名字 → (DNS 名集合, IP 集合)。"""
    dns: set[str] = set()
    ips: set[str] = set()
    try:
        san = cert.extensions.get_extension_for_class(x509.SubjectAlternativeName).value
    except x509.ExtensionNotFound:
        return dns, ips
    for value in san.get_values_for_type(x509.DNSName):
        dns.add(value.lower())
    for value in san.get_values_for_type(x509.IPAddress):
        ips.add(str(value))
    return dns, ips


def dns_covered(name: str, dns_names: set[str]) -> bool:
    """按 X.509 语义判断名字是否被覆盖。

    通配只吃最左一个标签，而且**不覆盖裸域** —— `*.baidu.com` 匹配
    `www.baidu.com`，不匹配 `baidu.com`。
    """
    if name in dns_names:
        return True
    labels = name.split(".")
    if len(labels) >= 3:
        return "*." + ".".join(labels[1:]) in dns_names
    return False


def mapping_matches(host: str, domain: str) -> bool:
    """当前访问的域名命中了映射表里的哪一条（和程序的命中规则一致）。"""
    if domain.startswith("*."):
        return host.endswith("." + domain[2:].strip("."))
    return host == domain


def missing_names(required: list[str], cert: x509.Certificate) -> list[str]:
    dns, ips = cert_names(cert)
    gone: list[str] = []
    for name in required:
        if is_ip(name):
            if str(ipaddress.ip_address(name.strip().strip("[]"))) not in ips:
                gone.append(name)
        elif not dns_covered(name, dns):
            gone.append(name)
    return gone


def days_left(cert: x509.Certificate) -> int:
    end = cert.not_valid_after_utc
    return int((end - datetime.now(timezone.utc)).total_seconds() // 86400)


# ---------------------------------------------------------------------------
# 信任状态（Windows 走 certutil，其它平台只给指引）
# ---------------------------------------------------------------------------


def _certutil(args: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["certutil", *args],
        capture_output=True,
        text=True,
        errors="replace",
        creationflags=0x0800_0000 if os.name == "nt" else 0,
    )


def ca_trusted(ca_cert: Path) -> bool | None:
    """Windows 上查当前用户根证书库；其它平台返回 None（表示"自己看"）。"""
    if os.name != "nt":
        return None
    result = _certutil(["-user", "-store", "Root"])
    if result.returncode != 0:
        return False
    return CA_COMMON_NAME in (result.stdout + result.stderr)


def trust_fix_hint(ca_cert: Path) -> str:
    system = platform.system()
    if system == "Windows":
        return f"装进当前用户信任库：python {Path(sys.argv[0]).name} --install-ca"
    if system == "Darwin":
        return (
            "取消信任 / 信任："
            f'sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain "{ca_cert}"'
        )
    return (
        "信任：把 ca.crt 复制到 /usr/local/share/ca-certificates/ 后执行 "
        "sudo update-ca-certificates"
    )


def install_ca(ca_cert: Path) -> None:
    if os.name != "nt":
        sys.exit(f"这个开关只处理 Windows；{trust_fix_hint(ca_cert)}")
    result = _certutil(["-user", "-f", "-addstore", "Root", str(ca_cert)])
    if result.returncode != 0:
        sys.exit(f"安装失败：{(result.stdout + result.stderr).strip()}")
    print(f"✓ 根证书已装入当前用户的受信任根证书颁发机构：{ca_cert}")


def uninstall_ca() -> None:
    if os.name != "nt":
        sys.exit("这个开关只处理 Windows")
    result = _certutil(["-user", "-delstore", "Root", CA_COMMON_NAME])
    if result.returncode != 0:
        sys.exit(f"取消信任失败：{(result.stdout + result.stderr).strip()}")
    print(f"✓ 已取消信任：{CA_COMMON_NAME}")


# ---------------------------------------------------------------------------
# 证书准备
# ---------------------------------------------------------------------------


class Prepared:
    """准备结果：证书文件 + 供页面展示的信息。"""

    def __init__(self, cert_path: Path, key_path: Path, ca_path: Path, cert: x509.Certificate,
                 reused: str | None, signed_for: list[str]):
        self.cert_path = cert_path
        self.key_path = key_path
        self.ca_path = ca_path
        self.cert = cert
        self.reused = reused  # 复用了谁；None 表示刚签的
        self.signed_for = signed_for


def prepare_certs(
    cert_dir: Path, required: list[str], mapped: list[str], force: bool
) -> Prepared:
    """把「能用的叶证书」准备好。

    优先复用程序签的那张（覆盖了全部映射名字就不重复劳动）；不够用就自签到
    `flask-https/`。无论哪条路径，都不写程序的文件。
    """
    ca_cert_path = cert_dir / "ca.crt"
    ca_key_path = cert_dir / "ca.key"
    if not (ca_cert_path.is_file() and ca_key_path.is_file()):
        print(f"· 程序还没生成过根 CA，脚本自己签一个：{cert_dir}")
        create_ca(cert_dir)

    ca_cert = _read_cert(ca_cert_path)
    ca_key = _read_key(ca_key_path)
    if ca_cert is None or ca_key is None:
        sys.exit(f"读不出根证书或私钥：{ca_cert_path}")

    program_cert = cert_dir / "server.crt"
    program_key = cert_dir / "server.key"
    if not force and program_cert.is_file() and program_key.is_file():
        existing = _read_cert(program_cert)
        if existing is not None and existing.issuer == ca_cert.subject:
            gone = missing_names(mapped, existing)  # 只看映射名字，回环名字不算硬要求
            if not gone and days_left(existing) > RENEW_BEFORE_DAYS:
                return Prepared(
                    program_cert, program_key, ca_cert_path, existing,
                    "程序签发的叶证书", [],
                )

    target = cert_dir / FLASK_CERT_DIR_NAME
    old = _read_cert(target / "server.crt")
    own_key = target / "server.key"
    if not force and old is not None and own_key.is_file():
        gone = missing_names(required, old)
        if not gone and days_left(old) > RENEW_BEFORE_DAYS and old.issuer == ca_cert.subject:
            return Prepared(target / "server.crt", own_key, ca_cert_path, old,
                            "上次脚本签的叶证书", [])

    leaf = sign_leaf(ca_cert, ca_key, required, target)
    return Prepared(target / "server.crt", own_key, ca_cert_path, leaf, None, required)


# ---------------------------------------------------------------------------
# Flask 应用
# ---------------------------------------------------------------------------

PAGE = """<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>本地 DNS + HTTPS 链路正常</title>
<style>
 :root {{ color-scheme: light dark; }}
 body {{ font: 15px/1.6 system-ui,-apple-system,"Segoe UI",sans-serif; margin:0;
        padding:28px 18px; background:#f6f7f9; color:#1c1e21; }}
 main {{ max-width:720px; margin:0 auto; }}
 h1 {{ font-size:19px; margin:0 0 4px; }}
 .ok {{ color:#0a7d3c; }} .bad {{ color:#b3261e; }}
 .card {{ background:#fff; border:1px solid #e3e5e8; border-radius:10px;
          padding:14px 16px; margin:14px 0; }}
 dl {{ display:grid; grid-template-columns:8.5em 1fr; gap:6px 12px; margin:0; }}
 dt {{ color:#5f6368; }} dd {{ margin:0; word-break:break-all; }}
 code {{ font-family:ui-monospace,Consolas,monospace; font-size:13px; }}
 ul {{ margin:6px 0 0; padding-left:20px; }}
 .hint {{ color:#5f6368; font-size:13px; }}
 @media (prefers-color-scheme: dark) {{
   body {{ background:#16181c; color:#e8eaed; }}
   .card {{ background:#1f2226; border-color:#33373c; }}
   dt {{ color:#9aa0a6; }} .hint {{ color:#9aa0a6; }}
   .ok {{ color:#5cd68a; }} .bad {{ color:#f28b82; }}
 }}
</style></head><body><main>
<h1>本地 DNS + HTTPS 链路{verdict}</h1>
<p class="hint">页面能打开就说明：域名解析到了这台机器、证书名字对得上、证书被浏览器信任。</p>

<div class="card"><dl>
 <dt>请求域名</dt><dd><code>{host}</code>{host_note}</dd>
 <dt>服务监听</dt><dd><code>{listen}</code></dd>
 <dt>叶证书</dt><dd>{leaf_summary}</dd>
 <dt>根证书</dt><dd>{ca_summary}</dd>
 <dt>证书文件</dt><dd><code>{cert_path}</code><br><code>{key_path}</code></dd>
 <dt>客户端</dt><dd>{user_agent}</dd>
 <dt>时间</dt><dd>{now}</dd>
</dl></div>

<div class="card"><b>证书覆盖的名字</b>
<ul>{name_items}</ul>
<p class="hint">{names_hint}</p></div>

<div class="card"><b>程序的本地 DNS 映射</b>
<ul>{mapping_items}</ul>
<p class="hint">映射由程序在系统里生效；本脚本读的是同一份 settings.json。</p></div>

<div class="card"><b>其它接口</b>
<ul><li><code>/healthz</code> — JSON 健康检查与证书摘要</li>
<li><code>/echo</code> — 回显请求头与查询串，方便按域名区分服务</li></ul></div>
</main></body></html>
"""


def build_app(prepared: Prepared, mappings: list[tuple[str, str]], data: Path,
              listen: str) -> Flask:
    app = Flask(__name__)
    dns_names, ip_names = cert_names(prepared.cert)
    all_names = sorted(dns_names | ip_names)
    mapped_domains = [domain for domain, _ in mappings]

    def summary() -> dict:
        return {
            "certFile": str(prepared.cert_path),
            "keyFile": str(prepared.key_path),
            "caFile": str(prepared.ca_path),
            "caCommonName": CA_COMMON_NAME,
            "notAfter": prepared.cert.not_valid_after_utc.date().isoformat(),
            "daysLeft": days_left(prepared.cert),
            "names": all_names,
            "mappings": [{"domain": d, "address": a} for d, a in mappings],
            "dataDir": str(data),
        }

    @app.get("/healthz")
    def healthz():
        return jsonify({"ok": True, **summary()})

    @app.get("/echo")
    def echo():
        return jsonify(
            {
                "host": request.host,
                "scheme": request.scheme,
                "path": request.path,
                "query": request.args.to_dict(),
                "headers": dict(request.headers),
            }
        )

    @app.get("/")
    def index():
        # 浏览器给的是带端口的主机名，比对名字时去掉端口。
        host = request.host.split(":")[0].strip("[]").lower()
        host_is_ip = is_ip(host)
        covered = (host in ip_names) if host_is_ip else dns_covered(host, dns_names)
        # 当前域名命中了映射表里的哪一条。
        hit = [d for d in mapped_domains if mapping_matches(host, d)]
        name_items = "".join(
            f"<li><code>{n}</code>{' ← 当前域名' if n == host else ''}</li>" for n in all_names
        )
        mapping_items = (
            "".join(
                f"<li><code>{d}</code> → <code>{a}</code>"
                f"{' ← 当前域名' if d in hit else ''}</li>"
                for d, a in mappings
            )
            or "<li>（程序里还没配置映射）</li>"
        )
        return (
            PAGE.format(
                verdict="" if covered else "（域名不在证书覆盖内）",
                host=request.host,
                host_note=""
                if covered
                else ' <span class="bad">这个域名不在证书的 SAN 里，浏览器本该报名字不匹配</span>',
                listen=listen,
                leaf_summary=(
                    f"主体 <code>{LEAF_COMMON_NAME}</code>，到期 "
                    f"<code>{prepared.cert.not_valid_after_utc.date()}</code>"
                    f"（剩 {days_left(prepared.cert)} 天）"
                ),
                ca_summary=f"<code>{CA_COMMON_NAME}</code>，见 <code>{prepared.ca_path}</code>",
                cert_path=prepared.cert_path,
                key_path=prepared.key_path,
                user_agent=request.headers.get("User-Agent", "-"),
                now=datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
                name_items=name_items,
                names_hint=(
                    "通配名字不覆盖裸域，所以 `*.baidu.com` 与 `baidu.com` 会各写一条。"
                    if any(n.startswith("*.") for n in all_names)
                    else "这些名字由程序按映射表推导，脚本不额外加工。"
                ),
                mapping_items=mapping_items,
            ),
            (200 if covered else 421),
        )

    return app


def serve_http_redirect(port: int, target_port: int) -> threading.Thread:
    """可选：80 端口上把 http 请求跳到 https，省得手输协议。"""
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

    class Redirect(BaseHTTPRequestHandler):
        def do_GET(self):  # noqa: N802 - 标准库命名
            host = (self.headers.get("Host") or "localhost").split(":")[0]
            suffix = "" if target_port == 443 else f":{target_port}"
            self.send_response(301)
            self.send_header("Location", f"https://{host}{suffix}{self.path}")
            self.end_headers()

        def log_message(self, *_args):  # 安静点
            pass

    server = ThreadingHTTPServer(("0.0.0.0", port), Redirect)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return thread


# ---------------------------------------------------------------------------
# 输出
# ---------------------------------------------------------------------------


def local_addresses() -> set[str]:
    """本机地址集合，用来判断某条映射是否指向这台机器。"""
    addrs = {"127.0.0.1", "::1", "localhost", "0.0.0.0", "::"}
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        probe.connect(("10.255.255.255", 1))  # 只查路由表，不发包
        addrs.add(probe.getsockname()[0])
    except OSError:
        pass
    finally:
        probe.close()
    return addrs


def report(prepared: Prepared, mappings: list[tuple[str, str]], port: int) -> None:
    dns, ips = cert_names(prepared.cert)
    mine = local_addresses()
    lines: list[str] = []
    for domain, address in mappings:
        mark = "本机" if address in mine else "其它主机"
        sample = f"www.{domain[2:]}" if domain.startswith("*.") else domain
        lines.append(f"   https://{sample}/{'' if port == 443 else '  (端口 ' + str(port) + ')'}   ← {domain} → {address} [{mark}]")

    print("\n" + "=" * 68)
    print(" 本地 HTTPS 示例服务已启动")
    print(f" 证书：{prepared.cert_path}")
    print(f"       覆盖 {len(dns | ips)} 个名字，到期 {prepared.cert.not_valid_after_utc.date()}"
          f"（剩 {days_left(prepared.cert)} 天）"
          + (f"，复用：{prepared.reused}" if prepared.reused else "，本次新签"))
    trusted = ca_trusted(prepared.ca_path)
    trusted_text = {True: "已装入当前用户信任库", False: "未装入信任库", None: "自行确认"}[trusted]
    print(f" 根证书：{prepared.ca_path}（{trusted_text}）")
    if trusted is False:
        print(f"         {trust_fix_hint(prepared.ca_path)}")
    print(f" 监听：0.0.0.0:{port}（Ctrl+C 退出）")
    if lines:
        print(" 可用地址（映射表里指向本机的才由本脚本服务）：")
        print("\n".join(lines))
    else:
        print(" 映射表为空，先用 https://localhost/ 验证本地链路")
    if missing_names(list(LOOPBACK_NAMES), prepared.cert):
        print(" 注意：这张证书没覆盖 localhost/127.0.0.1，"
              "https://localhost/ 会报名字不匹配，请用上面的映射域名访问")
        print("       （想让 localhost 也可用：加 --force 让脚本按映射重签一张）")
    print("=" * 68 + "\n")


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(
        description="复用程序的根 CA，按本地 DNS 映射准备证书并起一个 Flask HTTPS 示例服务",
    )
    parser.add_argument("--data-dir", help="程序数据目录（默认跟随 NETWORK_MANAGER_DATA_DIR）")
    parser.add_argument("--port", type=int, default=443, help="HTTPS 端口（默认 443）")
    parser.add_argument("--host", default="0.0.0.0", help="监听地址（默认 0.0.0.0，手机可访问）")
    parser.add_argument("--http-port", type=int, default=0, help="额外在 80 风格端口上做 301 跳转")
    parser.add_argument("--dns-file", help="改用程序导出的映射文本（一行一条「域名 地址」）")
    parser.add_argument("--name", action="append", default=[], help="额外要覆盖的名字，可重复")
    parser.add_argument("--force", action="store_true", help="忽略缓存，强制重签叶证书")
    parser.add_argument("--certs-only", action="store_true", help="只准备证书，不起服务")
    parser.add_argument("--check", action="store_true", help="只打印现状，不做任何写入")
    parser.add_argument("--install-ca", action="store_true", help="Windows：把根证书装进当前用户信任库")
    parser.add_argument("--uninstall-ca", action="store_true", help="Windows：取消信任")
    args = parser.parse_args()

    data = data_dir(args.data_dir)
    cert_dir = data / CERT_DIR_NAME

    if args.uninstall_ca:
        uninstall_ca()
        return

    mappings = (
        mappings_from_text(Path(args.dns_file))
        if args.dns_file
        else load_mappings(data)
    )
    mapped = required_names(mappings, [], loopback=False)  # 程序会给证书写的名字
    required = required_names(mappings, args.name)  # 脚本自己签的时候要多带回环名字
    listen = f"{args.host}:{args.port}"

    print(f"· 数据目录：{data}")
    for domain, address in mappings:
        print(f"· 映射：{domain} → {address}")
    if not mappings:
        print("· 映射为空，只覆盖回环名字（先在程序里配一条本地 DNS 映射再跑更完整）")

    if args.check:
        print(f"· 证书目录：{cert_dir}")
        ca_cert = cert_dir / "ca.crt"
        cert = _read_cert(cert_dir / "server.crt")
        if cert is not None:
            dns, ips = cert_names(cert)
            print(f"· 程序的叶证书：覆盖 {len(dns | ips)} 个名字，到期 "
                  f"{cert.not_valid_after_utc.date()}（剩 {days_left(cert)} 天）")
            gone = missing_names(mapped, cert)
            print("· 映射名字覆盖：" + ("全部覆盖" if not gone else f"缺 {gone}"))
        else:
            print("· 程序的叶证书：还没有")
        print(f"· 应覆盖的名字：{required}")
        if ca_cert.is_file():
            trusted = ca_trusted(ca_cert)
            print("· 根证书信任：" + {True: "已装入", False: "未装入", None: "自行确认"}[trusted])
            if trusted is False:
                print(f"  {trust_fix_hint(ca_cert)}")
        else:
            print("· 根证书：还没有（跑一次不带 --check 的脚本会生成）")
        return

    if args.install_ca:
        if not (cert_dir / "ca.crt").is_file():
            sys.exit(f"还没有根证书：{cert_dir / 'ca.crt'}，先跑一次脚本生成")
        install_ca(cert_dir / "ca.crt")
        return

    prepared = prepare_certs(cert_dir, required, mapped, args.force)
    if prepared.reused:
        print(f"· 复用{prepared.reused}：{prepared.cert_path}")
    else:
        print(f"· 已按映射表签出叶证书：{prepared.cert_path}")

    if args.certs_only:
        print(f"  ca.crt     {prepared.ca_path}")
        print(f"  server.crt {prepared.cert_path}")
        print(f"  server.key {prepared.key_path}")
        print("  拿去给 uvicorn / gunicorn / nginx 用都可以："
              "`--ssl-certfile server.crt --ssl-keyfile server.key`")
        return

    if args.http_port:
        serve_http_redirect(args.http_port, args.port)

    app = build_app(prepared, mappings, data, listen)
    report(prepared, mappings, args.port)
    try:
        app.run(
            host=args.host,
            port=args.port,
            ssl_context=(str(prepared.cert_path), str(prepared.key_path)),
            use_reloader=False,
            threaded=True,
        )
    except OSError as err:
        sys.exit(
            f"监听 {listen} 失败：{err}\n"
            f"端口被占用就换一个：--port 8443（映射与证书都不用改，只是访问时带端口）"
        )


if __name__ == "__main__":
    main()
