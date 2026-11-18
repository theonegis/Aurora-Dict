# 极光字典移动版：下载与安装

中文名 **极光字典**，英文名 **Aurora Dict**。移动版与桌面版的版本号统一为 **2.2.0**，安装包集中在[同一个 GitHub Release](https://github.com/theonegis/Aurora-Dict/releases/tag/v2.2.0)。本仓库仅分发移动端编译产物，不包含 Flutter 移动端源码。

## 下载安装包

| 平台 | 下载 | 系统要求与安装方式 |
| --- | --- | --- |
| Android | [Android 通用安装包 APK](https://github.com/theonegis/Aurora-Dict/releases/download/v2.2.0/aurora-dict_2.2.0_android_universal.apk) | Android 7.0 及以上，64 位 ARM 或 x86_64；下载后安装 |
| iPhone / iPad | [iOS 安装包 IPA（需自行签名）](https://github.com/theonegis/Aurora-Dict/releases/download/v2.2.0/aurora-dict_2.2.0_ios_arm64_unsigned.ipa) | iOS / iPadOS 15 及以上；借助 Windows 或 Mac 重新签名后安装 |
| 文件校验 | [SHA-256 校验值](https://github.com/theonegis/Aurora-Dict/releases/download/v2.2.0/aurora-dict_2.2.0_mobile_SHA256SUMS.txt) | 用于确认下载文件完整 |

两个平台原始包名均为 `cn.tzy.dict`，本次构建号为 `22000`。安装包内含完整 ECDICT 离线词库和本地 AI 推理引擎；AI 模型需在设置中另行下载。首次打开会初始化词库，请等待完成，无需联网下载词库。

## Android：直接安装

1. 用手机浏览器下载上表中的 `.apk` 文件。GitHub Release 中的 `Source code` 压缩包不是手机安装包。
2. 打开下载的 APK；如系统询问，允许当前浏览器或文件管理器“安装未知应用”，然后返回安装。
3. 打开极光字典，等待离线词库初始化完成，即可查询。

这份 APK 同时包含 `arm64-v8a` 和 `x86_64`，不支持仅能运行 32 位应用的手机。安装包约 289 MB，请为解压词库和后续模型留出足够空间。

**升级：**后续同签名版本可覆盖安装，通常会保留数据。如果装过此前使用测试签名的内部版本，系统可能提示“签名不一致”或“应用未安装”：先在旧版导出生词本 SQLite 副本，再卸载旧版、安装发布版并导入。不要在没有备份时卸载旧版。

发布签名证书 SHA-256 指纹：

```text
cf83949216c65299e88cb6b0caa67c58055bc5c99dc08c89e5361a3ed585ab7c
```

## iOS：用自己的 Apple 账号签名安装

**这个 IPA 不能像 APK 一样点开直接安装，也不是 App Store 或 TestFlight 安装包。** 它已移除作者的个人签名和设备描述文件，需要用安装者自己的 Apple 账号重新签名。普通免费 Apple 账号也可以使用，无需拥有 Mac，Windows 电脑同样可完成。

推荐使用 [AltStore Classic 与 AltServer](https://altstore.io/)。请使用 Classic 的 IPA 侧载流程，而非 AltStore PAL 的应用市场流程。

### 第一次安装

1. 在电脑安装官方 AltServer。按对应平台的官方教程准备环境：[Windows 教程](https://faq.altstore.io/altstore-classic/how-to-install-altstore-windows) / [macOS 教程](https://faq.altstore.io/altstore-classic/how-to-install-altstore-macos)。Windows 所需的 Apple 组件、安装来源及兼容要求以该教程为准。
2. 用数据线连接并解锁 iPhone，按提示“信任此电脑”；在 iTunes 或 Finder 中启用 Wi-Fi 同步。
3. 从电脑的 AltServer 菜单选择 **Install AltStore → 你的 iPhone**，按工具提示用自己的 Apple 账号完成安装。Apple 账号仅用于签名，不是极光字典的账号。
4. 按 iOS 提示在「设置 → 通用 → VPN 与设备管理」信任对应开发者；iOS 16 及以上还需要在「设置 → 隐私与安全性 → 开发者模式」开启并按提示重启。详见上述官方教程。
5. 在 iPhone 下载上表的 `.ipa`，保存到「文件」。打开 AltStore Classic 的 **My Apps** 页面，点 **＋** 并选择该 IPA；保持电脑 AltServer 运行且设备可连接，等待签名和安装完成。
6. 打开极光字典，等待词库初始化。

### 签名有效期与更新

免费账号侧载的 App 通常 **7 天到期**，需要在到期前通过 AltStore 的 **Refresh All** 续签；自动刷新依赖 AltStore、AltServer 及设备连接条件。免费账号同时激活的侧载 App 数量也有限制，详见 [AltStore 使用与刷新说明](https://faq.altstore.io/altstore-classic/your-altstore)。

更新时使用同一 Apple 账号、同一工具导入新版 IPA。签名工具可能为包名添加账号后缀；更换账号或安装工具可能造成另一份独立安装。更新前建议导出生词本。App 到期打不开时应先续签，而非直接删除。

本次已验证 IPA 的 Release 构建、版本号、设备架构、离线词库和包结构；未替用户在 AltStore 中登录 Apple 账号，因此不同账号的侧载流程仍需使用者按上述步骤完成。

## 与电脑同步生词本

1. 电脑安装 [Aurora Dict 2.2.0](https://github.com/theonegis/Aurora-Dict/releases/tag/v2.2.0)，在设置中启用局域网同步并生成配对二维码。
2. 手机和电脑连接同一局域网，保持电脑端运行；手机打开「设置 → 设备与同步」，扫码或粘贴配对信息。
3. 允许手机访问本地网络；完成配对后可手动同步，或启用打开 App 自动同步。

同步不需要注册极光字典账号。首次同步会合并两端生词；发生编辑冲突时保留冲突版本。网络隔离、VPN 或电脑防火墙可能影响连接。详细说明见 [README 的手机与电脑同步](README.md#手机与电脑同步)。

## 常见问题

- **下载中断或文件打不开：**重新下载并比对 SHA-256；不要把网页另存为安装包，也不要把 ZIP 扩展名改成 APK。
- **第一次查词需要等一会：**安装包正在解压和初始化词库，等待提示完成即可。
- **AI 查词、翻译或复习不可用：**先在「本地模型」设置中下载或导入模型；较大的模型需要更多存储和运行内存。
- **iOS 下载后没有安装按钮：**正常，需先通过签名工具安装，Safari 不能直接安装这个 IPA。
- **没有 Mac：**可用 Windows 与 AltServer。仅下载 IPA 到手机并不能绕过 iOS 的签名要求。

可在电脑检查下载文件完整性：macOS 使用 `shasum -a 256 文件名`，Linux 使用 `sha256sum 文件名`，Windows PowerShell 使用 `Get-FileHash 文件名 -Algorithm SHA256`，然后与 Release 附带的校验值比较。

[返回项目主页](README.md)
