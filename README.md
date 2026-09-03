# Network Manager

一个跨平台 TUN 分流管理器。Windows 使用一体化桌面窗口，Linux 以 systemd 无界面服务运行并通过 WebGUI 配置；Android 使用原生 `VpnService`。三端共用订阅、节点与分流配置格式。

每个发布版本必须同时提供新构建的 Windows、Android 正式 APK 与 Linux 成品，详细约束见 [`docs/RELEASE_POLICY.md`](docs/RELEASE_POLICY.md)。

## 当前能力

- 规则分流、全局 Clash、全局 v2ray、全局内置节点、智能节点和直连模式
- Clash YAML、Base64 订阅以及 VLESS、VMess、Trojan、Shadowsocks 等常见分享链接
- 兼容需要特定 User-Agent 的订阅，并支持 Clash `proxy-providers`
- 内置节点卡片选择、单节点测速与批量延迟测试；结果自动按低延迟优先、失败节点置后
- 智能节点每 60 秒执行真实 URL 健康检查；候选节点变慢、失效或出现明显更快节点时自动切换
- Google、ChatGPT、Claude、YouTube、GitHub 等常用海外站点预置规则组
- 始终位于规则末尾的强制保底规则，默认直连，可切换到本地端口或内置节点组
- 局域网、回环、链路本地和保留地址固定直连，并从 TUN 路由中排除，不受全局或保底规则影响
- 最近 60 秒上传、下载、累计流量和连接数监控
- 通过 SSH 自动部署独立 Shadowsocks 2022 服务，并生成可复制的跨设备节点
- 多端点出口 IP 检测，避免单个检测服务限流造成误报
- TUN 就绪检测、配置热加载、异常退出自动退避恢复，避免改规则时反复重建网卡
- Windows 与 Android 通用配置文件快速导入导出；SSH 凭据、桌面进程规则和本地端口不会跨设备导出
- 系统托盘、登录启动、桌面快捷方式和配置自动校验

Windows WebGUI 由仅监听 `127.0.0.1` 的会话令牌 API 提供，并通过 Qt WebEngine 直接嵌入 Network Manager 主窗口。界面、托盘和后台管理属于同一个应用实例，不会再打开 Edge 窗口。Linux 复用同一套页面，以无 Qt 的 HTTP 服务提供；远程监听额外强制 HTTP Basic 管理密码。Mihomo 在两端都作为受控子进程独立运行。

## Linux 无界面服务

Linux 版不安装桌面窗口，支持 amd64 与 arm64。它复用 Windows 的 WebGUI、Mihomo TUN、订阅与节点管理、规则组、智能节点、测速、流量监控和跨设备配置，但不包含 SSH 服务器部署功能。

```bash
sudo bash apps/linux/install.sh
```

安装后由 `network-manager.service` 常驻。首次安装只启动 WebGUI，导入节点并确认测速后再启动 TUN；启用“打开后自动接管”后，服务重启会自动恢复。WebGUI 安全默认只监听 `127.0.0.1:9091`，安装程序会生成随机管理密码；远程访问可使用 SSH 端口转发，或自行配置带 HTTPS 的反向代理。安装位置、服务命令和直接监听方法见 `apps/linux/README.md`。

## Windows 开发运行

需要 Python 3.10+。TUN 启动需要管理员权限。

```powershell
python -m pip install -e ".[dev]"
python scripts/download_mihomo.py
powershell -ExecutionPolicy Bypass -File scripts/run_windows_admin.ps1
```

普通权限也可执行 `python -m network_manager` 查看和编辑配置，但不能启动 TUN 接管。

## 服务器代理部署

在“服务器部署”页面填写 Linux 服务器 IP/域名、SSH 端口、用户名、远端代理端口和认证方式，然后点击“部署代理”。新配置会生成一个 `10000` 以上的随机默认部署端口，可在“设置”中手动修改或重新随机。已部署服务的端口与当前默认值不一致时，会在“检查服务”时迁移到当前默认端口。当前自动部署要求服务器使用 systemd 且 SSH 用户为 `root`。

程序会安装经过固定 SHA-256 校验的 sing-box，使用独立的 `/etc/network-manager-proxy` 配置和 `network-manager-proxy.service`，不会覆盖服务器已有的 sing-box 配置。部署完成后 SSH 会断开，代理服务由 systemd 独立运行；生成的节点会自动加入“内置节点”，也可复制 `ss://` 链接到 Windows、macOS 或 Android 的兼容客户端。

代理端口会监听公网 TCP/UDP。程序会处理已启用的 `ufw` 或 `firewalld`，但云厂商安全组仍需自行确认放行。重新部署会轮换节点密码并重启服务；若重启失败，会恢复旧配置。删除本地服务器记录不会卸载远端服务。

Windows 下“记住凭据”使用当前登录用户的 DPAPI 加密，SSH 明文密码不会写入 `settings.json`。首次连接的主机密钥会保存到应用数据目录，之后密钥变化会被拒绝。代理节点自身的连接参数与其他导入节点一样保存在用户配置中，请勿公开分享配置文件。

## 构建 Windows 包

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build_windows.ps1
```

输出位于 `dist/NetworkManager/`。构建为稳定的一目录包并带管理员清单；Mihomo 作为独立子进程运行，主程序异常退出时 Windows Job Object 会清理核心。

正式安装包使用 Inno Setup 7 构建，默认安装到当前用户目录并创建开始菜单和桌面快捷方式：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build_windows_installer.ps1
```

输出位于 `release-assets/v<版本>/NetworkManager-Setup-x64-v<版本>.exe`，支持覆盖升级和标准卸载。卸载不会删除 `%LOCALAPPDATA%\NetWorkManger` 中的用户配置。

构建脚本会为当前用户创建桌面快捷方式。也可单独执行：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/create_desktop_shortcut.ps1
```

## Android 开发与测试

Android 原生客户端位于 `apps/android/`，使用系统 `VpnService` 和 sing-box libbox。它支持与 Windows 相同的订阅、节点、常用海外站点规则、强制保底、智能节点、实时流量及通用配置导入导出，但不包含 SSH 服务器部署。

```powershell
cd apps/android
.\scripts\fetch_libbox.ps1
.\gradlew.bat testDebugUnitTest assembleDebug assembleDebugAndroidTest
```

连接模拟器或 Android 设备后，可按 `apps/android/README.md` 运行设备测试。通用配置字段见 `docs/portable-config-v1.md`。

## 测试

```powershell
python -m ruff check src tests scripts
python -m pytest -q
node --check src/network_manager/web/app.js
```

启动程序后还可以对本地 WebGUI 执行浏览器冒烟测试：

```powershell
python scripts/smoke_webgui.py http://127.0.0.1:<port>/ --poll-seconds 10
```

测试覆盖配置迁移、订阅格式、Mihomo 配置、实时流量计算、本地 API 鉴权、Windows 凭据加密、服务器部署配置和分享链接回读。

## 数据与安全

- 用户配置：`%LOCALAPPDATA%\NetWorkManger\settings.json`
- 生成的 Mihomo 配置：`%LOCALAPPDATA%\NetWorkManger\mihomo-config.yaml`
- 日志：`%LOCALAPPDATA%\NetWorkManger\logs`
- SSH 主机密钥：`%LOCALAPPDATA%\NetWorkManger\ssh-known-hosts`
- 加密 SSH 凭据：`%LOCALAPPDATA%\NetWorkManger\ssh-credentials.json`

Linux 用户配置位于 `/var/lib/network-manager`，WebGUI 凭据位于 `/etc/network-manager/network-manager.env`，运行日志通过 `journalctl -u network-manager` 查看。

仓库不包含本地配置、订阅地址、节点凭据、构建产物或 Mihomo 二进制。`scripts/download_mihomo.py` 会下载并校验固定版本的官方 Mihomo 包。

## 平台范围

Windows 11 是当前稳定平台。Linux 无界面版已经实现 systemd 服务、WebGUI 与 amd64/arm64 安装流程，进入测试阶段；Android 原生客户端已通过 API 36 模拟器的 VPN 接管与设备测试。macOS 与 iOS 目录已建立，但系统扩展、TUN 权限、签名和打包仍待实现。平台边界见 `PLATFORMS.md`。

项目界面和核心管理思路参考了 [Clash Verge Rev](https://github.com/clash-verge-rev/clash-verge-rev)，代理核心使用 [Mihomo](https://github.com/MetaCubeX/mihomo)。第三方许可见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
