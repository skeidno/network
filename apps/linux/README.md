# Linux Headless

Linux 版不创建桌面窗口。它以 systemd 服务运行 Mihomo TUN 核心，并复用与 Windows 相同的 WebGUI、规则、订阅、节点、智能切换、测速和流量监控。SSH 服务器部署页面不会在 Linux WebGUI 中显示。

## 支持范围

- x86_64 / amd64
- aarch64 / arm64
- Python 3.10+
- systemd
- `/dev/net/tun`

安装脚本固定使用 Mihomo `v1.19.30`，安装前必须通过仓库记录的 SHA-256 校验。
GitHub Release 中按架构提供的安装包已经包含 Mihomo 和 Python wheel，不依赖服务器现场下载核心。
Ubuntu/Debian 缺少 `python3-venv` 时，安装脚本会通过 `apt-get` 自动补齐。

## 安装

发布包安装（推荐）：

```bash
tar -xzf NetworkManager-Linux-amd64-vX.Y.Z.tar.gz
cd NetworkManager-Linux-amd64-vX.Y.Z
sudo ./install.sh
```

ARM64 服务器使用文件名包含 `arm64` 的安装包。

源码安装：

在源码目录执行：

```bash
sudo bash apps/linux/install.sh
```

脚本会安装到 `/opt/network-manager`，配置保存在 `/var/lib/network-manager`，服务名为 `network-manager.service`。重复运行安装脚本会更新程序；已有 WebGUI 账号密码会保留，Mihomo 核心校验一致时不会替换。

首次安装只启动 WebGUI，不会在没有可用节点时直接修改服务器路由。登录后先导入 Windows/Android 通用配置或添加订阅、完成节点测速，再点击“启动接管”。需要服务器重启后自动恢复 TUN 时，在设置中启用“打开后自动接管”。

默认 WebGUI 只监听 `127.0.0.1:9091`。从自己的电脑访问服务器时，先建立 SSH 转发：

```bash
ssh -L 9091:127.0.0.1:9091 user@server
```

然后打开 `http://127.0.0.1:9091/`，使用安装时输出的账号密码登录。忘记密码可由服务器管理员查看 `/etc/network-manager/network-manager.env`。

如需直接远程监听，可在环境文件中把 `NETWORK_MANAGER_WEB_HOST` 改成 `0.0.0.0` 后重启服务。必须保留强密码，并建议在前面配置 HTTPS 反向代理；安装脚本不会自动开放主机防火墙或云安全组。

```bash
sudo systemctl restart network-manager
sudo journalctl -u network-manager -f
```

## 手动开发运行

```bash
python3 -m venv .venv
. .venv/bin/activate
pip install -e .
NETWORK_MANAGER_WEB_PASSWORD='replace-this-password' \
  network-manager-headless --listen 127.0.0.1 --port 9091
```

启动 TUN 需要 root 或等效的 `CAP_NET_ADMIN` / `CAP_NET_RAW` 权限。WebGUI 服务可以普通用户启动，但此时无法启动全流量接管。
