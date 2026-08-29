# Network Manager

一个 Windows 优先的 TUN 分流管理器。它使用 Mihomo 接管流量，提供独立 WebGUI、订阅与节点管理、实时流量监控、规则组和 SSH 服务器出口。

## 当前能力

- 规则分流、全局 Clash、全局 v2ray、全局内置节点、全局 SSH 和直连模式
- Clash YAML、Base64 订阅以及 VLESS、VMess、Trojan、Shadowsocks 等常见分享链接
- 兼容需要特定 User-Agent 的订阅，并支持 Clash `proxy-providers`
- 内置节点卡片选择与批量延迟测试
- Google、ChatGPT、Claude、YouTube、GitHub 等常用海外站点预置规则组
- 最近 60 秒上传、下载、累计流量和连接数监控
- SSH 密码、私钥或 SSH Agent 认证，本地 SOCKS5 隧道和出口 IP 检测
- 系统托盘、登录启动和配置自动校验

WebGUI 由仅监听 `127.0.0.1` 的会话令牌 API 提供，使用系统 Edge 应用窗口显示。网页渲染器与 Mihomo/托盘后台相互独立，关闭或崩溃网页不会结束代理核心。

## Windows 开发运行

需要 Python 3.11+。TUN 启动需要管理员权限。

```powershell
python -m pip install -e ".[dev]"
python scripts/download_mihomo.py
powershell -ExecutionPolicy Bypass -File scripts/run_windows_admin.ps1
```

普通权限也可执行 `python -m network_manager` 查看和编辑配置，但不能启动 TUN 接管。

## SSH 服务器出口

在“SSH 服务器”页面填写服务器 IP/域名、SSH 端口、用户名、本地 SOCKS5 端口和认证方式。连接后可在规则或全局模式中选择“SSH 服务器”。

该功能使用 SSH `direct-tcpip` 通道，不会在远端安装软件，也不会向公网开放代理端口。Windows 下“记住凭据”使用当前登录用户的 DPAPI 加密；明文密码不会写入 `settings.json`。首次连接的主机密钥会保存到应用数据目录，之后密钥变化会被拒绝。

## 构建 Windows 包

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build_windows.ps1
```

输出位于 `dist/NetworkManager/`。构建为稳定的一目录包并带管理员清单；Mihomo 作为独立子进程运行，主程序异常退出时 Windows Job Object 会清理核心。

## 测试

```powershell
python -m ruff check src tests scripts
python -m pytest -q
node --check src/network_manager/web/app.js
```

启动后台后还可以运行实际 Edge WebGUI 冒烟测试：

```powershell
python scripts/smoke_webgui.py http://127.0.0.1:<port>/ --poll-seconds 10
```

测试覆盖配置迁移、订阅格式、Mihomo 配置、实时流量计算、本地 API 鉴权、Windows 凭据加密以及 SOCKS5 数据转发。

## 数据与安全

- 用户配置：`%LOCALAPPDATA%\NetWorkManger\settings.json`
- 生成的 Mihomo 配置：`%LOCALAPPDATA%\NetWorkManger\mihomo-config.yaml`
- 日志：`%LOCALAPPDATA%\NetWorkManger\logs`
- SSH 主机密钥：`%LOCALAPPDATA%\NetWorkManger\ssh-known-hosts`
- 加密 SSH 凭据：`%LOCALAPPDATA%\NetWorkManger\ssh-credentials.json`

仓库不包含本地配置、订阅地址、节点凭据、构建产物或 Mihomo 二进制。`scripts/download_mihomo.py` 会下载并校验固定版本的官方 Mihomo 包。

## 平台范围

Windows 11 是当前完整支持和测试的平台。核心模型、本地 WebGUI 和 SSH 隧道使用跨平台实现，为 macOS 客户端保留了迁移路径；macOS 的权限、TUN 打包和启动项仍需单独适配。Android 需要原生 VPNService 外壳，不在当前桌面版本支持范围内。

项目界面和进程隔离思路参考了 [Clash Verge Rev](https://github.com/clash-verge-rev/clash-verge-rev)，代理核心使用 [Mihomo](https://github.com/MetaCubeX/mihomo)。第三方许可见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
