# 构建 Aurora Dict

本文说明如何在 **macOS、Windows 与 Linux 本机**将 Aurora Dict 编译为可执行程序和可分发安装包。

> 请在目标操作系统上构建对应安装包。当前发布目标仅包括 macOS ARM64/x64、Windows x64 和 Linux x64。

## 1. 获取源码与离线词库

本项目的离线 ECDICT SQLite 资源由 Git LFS 管理。它是应用首次启动时准备本地字典所必需的文件；未下载它时，Tauri 打包会失败。

```bash
git clone https://github.com/theonegis/Aurora-Dict.git
cd aurora-dict

git lfs install
git lfs pull
```

确认资源已真实下载，而不是一个几行的 LFS 指针文件：

```bash
ls -lh src-tauri/resources/ecdict-sqlite-28.zip
git lfs ls-files
```

`ecdict-sqlite-28.zip` 的体积约为 207 MB。若文件很小或不存在，请重新执行 `git lfs pull`。

## 2. 通用工具链

项目当前要求：

- Node.js 22 LTS（Node.js 20+ 亦可用于本地开发）；
- Rust 1.77.2 或更新版本，使用 stable toolchain；
- Git 与 Git LFS；
- 目标平台所需的 Tauri 系统依赖。

安装 Rust（未安装时）：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
rustc --version
```

安装 JavaScript 与 Rust 依赖：

```bash
npm ci
```

构建前可运行以下检查：

```bash
npm run build
cd src-tauri
cargo test --lib
cd ..
```

启动开发版桌面程序：

```bash
npm run tauri dev
```

## 3. macOS

### 前置条件

安装 Xcode Command Line Tools：

```bash
xcode-select --install
```

确认 Node、Rust 与 Git LFS 已按上文安装。

### 构建 ARM64 DMG

```bash
rustup target add aarch64-apple-darwin
bash scripts/prepare-llama-sidecar.sh aarch64-apple-darwin
npm run tauri build -- --target aarch64-apple-darwin --bundles dmg
```

产物位于：

```text
src-tauri/target/aarch64-apple-darwin/release/bundle/dmg/
```

### 构建 x64 DMG

```bash
rustup target add x86_64-apple-darwin
bash scripts/prepare-llama-sidecar.sh x86_64-apple-darwin
npm run tauri build -- --target x86_64-apple-darwin --bundles dmg
```

产物位于：

```text
src-tauri/target/x86_64-apple-darwin/release/bundle/dmg/
```

### macOS Dock 图标

默认打包继续使用原有 `icon.icns`，保留已确认合适的 macOS 27 显示效果。针对 macOS 15 上图标偏大的情况，提供显式选择的兼容打包配置；它不按 Intel/Apple Silicon 或屏幕尺寸自动切换。

为 macOS 15 的 Intel Mac 构建时：

```bash
bash scripts/prepare-llama-sidecar.sh x86_64-apple-darwin
npm run tauri build -- --target x86_64-apple-darwin --bundles dmg --config src-tauri/tauri.macos-legacy.conf.json
```

此配置仅替换安装包图标，也适用于应用未运行时的 Dock 显示。其他 macOS 窗口配置仍由 `tauri.macos.conf.json` 提供。默认构建和发布工作流不变；兼容包不是一个会在运行时自动适配系统版本的通用包。兼容包与默认包使用相同应用标识及产物名称，请分开保存，避免混淆。

兼容图标 `icon-macos-legacy.icns` 从共享矢量源 `src-tauri/icons/aurora.svg` 居中缩放至 91%，使主体占画布约 84%，各档普通和 Retina 分辨率使用相同留白。比例由 `scripts/generate-macos-icon.mjs` 中的 `scale` 控制。修改 SVG 或比例后运行：

```bash
npm ci
npm run icons:macos
```

提交重新生成的 `src-tauri/icons/icon-macos-legacy.icns`，再构建兼容包。此命令不覆盖默认 `icon.icns`，也不改变 Windows、Linux 或应用内图标。

验证时，在 macOS 15 安装兼容包，macOS 27 使用默认包，对比 Dock 中相邻图标的可见尺寸，并检查运行/退出及不同 Dock 大小下的表现。若 Dock 仍保留旧图标，可先从 Dock 移除该项目，再从新安装的应用重新添加。跨系统视觉效果需要实机确认。

### 签名与公证

本地构建的应用可用于个人测试。若要将 DMG 交给其他 macOS 用户，则应使用 Apple Developer 的 **Developer ID Application** 证书签名并完成 notarization；否则 Gatekeeper 可能提示应用“已损坏”或无法验证开发者。

## 4. Windows

### 前置条件

1. 安装 [Node.js 22 LTS](https://nodejs.org/) 与 Git for Windows。
2. 安装 [Rust](https://rustup.rs/) 的 `stable-x86_64-pc-windows-msvc` 工具链。
3. 安装 Visual Studio 2022 Build Tools，勾选 **Desktop development with C++** 与 MSVC 工具集。
4. 确保系统已安装 Microsoft Edge WebView2 Runtime（Windows 11 通常已自带）。

在 PowerShell 中确认环境：

```powershell
node --version
rustc --version
git lfs version
```

### 构建 x64 NSIS 安装器

```powershell
npm ci
git lfs pull
bash scripts/prepare-llama-sidecar.sh x86_64-pc-windows-msvc
npm run tauri build -- --bundles nsis
```

产物位于：

```text
src-tauri\target\release\bundle\nsis\
```

其中的 `*-setup.exe` 即 Windows 安装程序。

## 5. Linux

Tauri 2 使用 WebKitGTK 4.1。请按发行版安装依赖后再执行构建。

### Debian / Ubuntu

```bash
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev \
  build-essential \
  curl wget file \
  libxdo-dev libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  patchelf rpm

npm ci
git lfs pull
bash scripts/prepare-llama-sidecar.sh x86_64-unknown-linux-gnu
npm run tauri build -- --bundles deb,rpm
```

### Fedora

```bash
sudo dnf install -y \
  webkit2gtk4.1-devel \
  openssl-devel \
  curl wget file \
  libappindicator-gtk3-devel \
  librsvg2-devel \
  libxdo-devel \
  gcc gcc-c++ make \
  dpkg

npm ci
git lfs pull
bash scripts/prepare-llama-sidecar.sh x86_64-unknown-linux-gnu
npm run tauri build -- --bundles rpm,deb
```

### Arch Linux

```bash
sudo pacman -Syu --needed \
  webkit2gtk-4.1 \
  base-devel \
  curl wget file openssl \
  appmenu-gtk-module libappindicator-gtk3 \
  librsvg xdotool dpkg rpm-tools

npm ci
git lfs pull
bash scripts/prepare-llama-sidecar.sh x86_64-unknown-linux-gnu
npm run tauri build -- --bundles deb,rpm
```

Linux 安装包产物默认位于：

```text
src-tauri/target/release/bundle/deb/
src-tauri/target/release/bundle/rpm/
```

本机可执行文件（不含安装包）位于：

```text
src-tauri/target/release/aurora-dict
```

面向 Arch Linux 用户发布时使用 `aurora-dict-bin` AUR 包，而不是把 deb 或 rpm 直接交给用户。

## 6. 仅构建可执行文件

若只需要测试二进制文件、不生成安装包：

```bash
npm run tauri build -- --no-bundle
```

常见输出位置：

| 平台 | 可执行文件 |
| --- | --- |
| macOS | `src-tauri/target/release/bundle/macos/Aurora Dict.app` |
| Windows | `src-tauri\\target\\release\\aurora-dict.exe` |
| Linux | `src-tauri/target/release/aurora-dict` |

## 7. 常见问题

### `resource path 'resources/ecdict-sqlite-28.zip' doesn't exist`

Git LFS 没有下载离线词库。执行：

```bash
git lfs install
git lfs pull
```

然后确认 `src-tauri/resources/ecdict-sqlite-28.zip` 存在且约 207 MB。

### Linux 提示找不到 WebKitGTK、GTK 或 AppIndicator

请重新执行本章中与你的发行版相对应的系统依赖安装命令。Tauri 2 需要 WebKitGTK 4.1 开发包。

### Windows 提示找不到链接器或 MSVC

安装或修复 Visual Studio Build Tools 的“Desktop development with C++”工作负载，然后重新打开 PowerShell 再运行构建。

### macOS 下载的 DMG 无法被其他用户打开

这是签名与 notarization 问题，而不是离线词库问题。发布给其他用户前需要配置 Apple Developer 证书、签名与公证。

## 8. GitHub Actions 构建全部平台

仓库已提供 [`.github/workflows/release.yml`](.github/workflows/release.yml)。它会在推送 `v*` 格式 Git tag 后构建：

- macOS ARM64 与 x64 DMG；
- Windows x64 NSIS 安装器；
- Linux x64 的 DEB 与 RPM。

发布新版本前，请确保 `src-tauri/tauri.conf.json` 的 `version` 与 tag 一致：

```bash
git tag -a v1.1.0 -m "Aurora Dict v1.1.0"
git push origin v1.1.0
```

工作流会用 Git LFS 拉取离线词库；如果 Actions 日志提示资源不存在，请确认该 ZIP 已作为 LFS 文件提交并已上传。

## 参考资料

- [Tauri 前置条件](https://v2.tauri.app/zh-cn/start/prerequisites/)
- [Tauri 发行与打包](https://v2.tauri.app/distribute/)
- [Tauri macOS DMG](https://v2.tauri.app/distribute/dmg/)
- [Tauri Windows 安装器](https://v2.tauri.app/distribute/windows-installer/)