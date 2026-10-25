# Aurora Dict

Aurora Dict 是一款为 Linux 用户而做、也同样适用于 macOS 与 Windows 的中英桌面词典。macOS 版本最低支持 macOS 15 Sequoia。

许多 Linux 上可用的词典要么界面陈旧，要么必须依赖网页或复杂的导入流程。这个项目希望提供另一种体验：打开即可使用的离线词库、清晰可靠的中英查询，以及一个安静、精致、不打扰阅读的桌面界面。它使用 Tauri 2 构建，前端采用 React、TypeScript 与 Vite；在保持轻量的同时，将 iOS 的卡片层次与 Windows Mica 的半透明质感带到桌面端。

## 特性

- Windows、macOS 与 Linux 的原生桌面窗口，自定义标题栏和统一的窗体圆角。
- 安装包内置 ECDICT SQLite 离线词库；首次启动自动准备完成，无需用户导入。
- 中文查英文、英文查中文、英文拼写建议，以及英式/美式音标与发音。
- 五个可配置来源：本地 ECDICT、本地 LLM、有道词典、[FreeDictionaryAPI.com](https://freedictionaryapi.com/) 与 [Merriam-Webster API](https://dictionaryapi.com/)；最多同时启用四个，默认启用前四个。
- 每次查询会并行获取并缓存启用来源的结果；默认保存最近 100 个单词，可在设置中调整。
- 独立 SQLite 生词本：查询结果或选中文本可加入，释义、例句与笔记可在生词本中使用 Markdown 编辑；支持自定义保存路径、打开、导入和导出，方便跨设备拷贝使用。
- 可调中英文界面、主题色、字体、缩放、透明度与材质模糊度。

有道词典与 FreeDictionaryAPI.com 不需要 API Key；Merriam-Webster 需要用户在设置页填写自己的 API Key。FreeDictionaryAPI.com 使用 English Wiktionary 的结构化数据（CC BY-SA 4.0）；网络不可用时，本地词典仍可独立工作。

## 开发

前提：Node.js 20+、Rust（MSRV 1.77.2），以及目标平台所需的 [Tauri 依赖](https://v2.tauri.app/start/prerequisites/)。

```bash
npm install
npm run tauri dev
```

仅预览前端：

```bash
npm run dev
```

浏览器预览没有 Tauri 后端，因此会以 `serendipity` 的静态体验数据演示本地卡片；真正的 SQLite 查询、系统窗口控制、字体枚举和在线请求均在桌面应用中运行。

界面文案与本地 AI 默认提示词已从代码中分离：

- `config/locales.json`：简体中文和英文界面文案；两种语言使用相同的键名。
- `config/prompts.json`：词典查询和中英翻译的默认 System Prompt。前端默认值与 Rust 后端兜底值读取同一文件。

修改这些配置后需要重新构建应用。用户在设置中自行编辑并保存的提示词仍会优先于内置默认值。

## 许可证

自 v0.1.2 起，Aurora Dict 的代码采用 [PolyForm Noncommercial License 1.0.0](LICENSE)：允许许可证所定义的非商业使用，但传播代码或衍生作品时必须保留许可证和其中的 `Required Notice` 署名声明。商业使用必须事先取得版权所有者的书面授权，详情见 [商业授权说明](COMMERCIAL-LICENSE.md)。此前已经依照 GPL-3.0 发布的版本仍受其原许可证约束。

Aurora Dict 是源代码可用（source-available）软件，并非 OSI 定义的开源软件。

## 离线词库许可证

`src-tauri/resources/ecdict-sqlite-28.zip` 是随安装包分发的 ECDICT SQLite 资源。应用首次启动会将它解压到系统应用数据目录并建立全文索引。完整的 ECDICT 许可证位于 [src-tauri/resources/ECDICT-LICENSE.txt](src-tauri/resources/ECDICT-LICENSE.txt)。

## 打包与发布

```Shell
bash scripts/prepare-llama-sidecar.sh aarch64-apple-darwin
npm run tauri build
```

推送新的 Git tag（或手动运行 Actions）会触发 [发布工作流](.github/workflows/release.yml)。工作流先创建草稿 Release，只有所有平台均构建成功才会公开发布：

```Shell
git tag -a v2.1.0 -m "Aurora Dict v2.1.0"
git push origin v2.1.0
```

- macOS ARM64 与 x64 `.dmg`；
- Windows x64 NSIS `.exe`；
- Linux x64 `.deb` 与 `.rpm`。

所有安装包默认包含 llama.cpp 本地推理引擎；Qwen 模型会由用户在设置页按需下载，因此不会增大初始安装包数百 MB。侧车二进制由 `scripts/prepare-llama-sidecar.sh` 准备。

Arch Linux 用户包使用 `aurora-dict-bin`。发布后可通过 `paru -S aurora-dict-bin` 安装。

工作流默认产生未经过开发者证书签名和公证的产物；macOS 安装包使用 ad-hoc 签名。

macOS下提示：“Aurora Dict”已损坏，无法打开。 你应该将它移到废纸篓，可在终端执行如下命令解决：

```Shell
xattr -rd com.apple.quarantine "/Applications/Aurora Dict.app"
```

## 支持作者

创作不易，如果您觉得该软件对您的工作学习有所帮助，请考虑打赏作者支持他继续完善该工具。您可以使用微信或支付宝扫码支持：

<p>
  <img src="src-tauri/resources/IMG_2816.JPG" alt="微信付款码" width="260">
  <img src="src-tauri/resources/IMG_2817.JPG" alt="支付宝付款码" width="260">
</p>

## 在线来源说明

FreeDictionaryAPI.com 数据源通过以下无需 API Key 的接口获取 English Wiktionary 结构化数据：

```text
https://freedictionaryapi.com/api/v1/entries/en/<word>
```

返回内容遵循其响应中标注的 CC BY-SA 4.0 许可；应用会保留 Wiktionary 来源说明。

有道内容会在 Rust 后端完成结构化提取；其免 Key 发音端点为：

```text
http://dict.youdao.com/dictvoice?type=0&audio=<word>  # 美式
http://dict.youdao.com/dictvoice?type=1&audio=<word>  # 英式
```

在线页面结构、访问策略或限流发生变化时，应用会明确显示错误，且不会影响本地查询。
