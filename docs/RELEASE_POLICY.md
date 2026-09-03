# 发布版本产物声明

从本声明生效后，每次创建新的 Git 标签或 GitHub Release，都必须同时重新构建并发布 Windows、Android 和 Linux 三端成品。即使改动只涉及其中一个平台，也不得只发布单端产物，不得复用或改名沿用旧版本文件。

## 必须包含的产物

假设发布版本为 `vX.Y.Z`，Release 至少必须包含：

- `NetworkManager-Setup-x64-vX.Y.Z.exe`：Windows x64 正式安装包。
- `NetworkManager-Android-vX.Y.Z.apk`：Android 正式签名 APK，禁止使用 debug APK。
- `NetworkManager-Linux-amd64-vX.Y.Z.tar.gz`：Linux amd64 无界面 WebGUI 服务包。
- `NetworkManager-Linux-arm64-vX.Y.Z.tar.gz`：Linux arm64 无界面 WebGUI 服务包。
- `SHA256SUMS.txt`：覆盖上述全部成品的 SHA-256 校验值。
- GitHub 自动生成的源码 `zip` 和 `tar.gz` 快照。

## 一致性要求

- 三端成品必须由同一个发布标签和同一个提交构建。
- 应用界面、包元数据、文件名、发布标题与 Git 标签必须使用相同的 `X.Y.Z` 版本号。
- 每次发布都必须生成新的三端成品；不允许把上一版本的 APK、Linux 包或 Windows 包挂到新 Release。
- Windows Release 统一提供安装包，不再同时发布免安装 ZIP，避免用户面对两个功能重复的下载项。
- 正式发布前必须分别验证 Windows 安装与启动、Android 正式 APK 安装与 VPN 接管、Linux 安装与 WebGUI/代理接管。
- 任一必需产物缺失、版本号不一致或基本验证失败时，不得把 Release 标记为正式发布。

此规则同样适用于补丁版本。平台专项修复可以只修改对应代码，但发布时仍须重新构建和验证全部三端产物。
