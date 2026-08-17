<div align="center">

# 🗂️ TidyMedia

**Windows 上极速的媒体管理器 + 照片库整理工具。**
索引整块硬盘，输入即搜索，安全地理清 15 年散落各处的照片。

[![CI](https://github.com/tienrocker/tidymedia/actions/workflows/ci.yml/badge.svg)](https://github.com/tienrocker/tidymedia/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/tienrocker/tidymedia?include_prereleases&label=release)](https://github.com/tienrocker/tidymedia/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078d6)
![Built with](https://img.shields.io/badge/built%20with-Tauri%202%20%2B%20Rust-orange)

[English](README.md) · [Tiếng Việt](README.vi.md) · **中文**

</div>

---

照片和视频经年累月地堆积 - iPhone 通过十几个不同的应用同步、相机、聊天软件 - 硬盘里塞满了重复副本和相互冲突的文件名（三部手机拍出的 `IMG_1234.JPG`）。TidyMedia 以**内容作为身份**，从不信任文件名，围绕三大支柱构建：

1. **整盘即时浏览与搜索** - index-first 架构；浏览时 UI 从不触碰文件系统。
2. **安全去重** - 分层哈希（size → xxh3 → 完整 BLAKE3），并排对比 + 同步缩放审阅，只删到回收站。*（感知相似度将在 M7 到来）*
3. **归整照片库** - 全部整理进你自己选择的库文件夹，目录结构和文件名都是可自定义模板（默认 `YYYY\YYYY-MM`），按拍摄日期命名、绝不冲突，强制 dry-run，每批可撤销。

## ✨ 亮点

- ⚡ **实测够快** - 真实硬盘上 218,000 个文件 **10 秒内**完成索引；*扫描还在进行时*搜索就能在 **20-40 ms** 内出结果。
- 🔎 **宽容的搜索** - 子串匹配、忽略音调符号（`anh tet` 能搜到 `Ảnh Tết`），可按类型 / 大小 / 日期 / 分辨率过滤。
- 🖼️ **缩略图网格 & 灯箱** - 完全虚拟化的网格，百万条目流畅滚动，WebP 缩略图缓存（2 GB LRU），以光标为中心缩放平移，EXIF 信息面板（拍摄时间、相机、尺寸）。HEIC/AVIF 经 ffmpeg 解码。
- 🐌 **对慢速磁盘友好** - 只为真正出现在屏幕上的项目请求缩略图，并优先处理最新的请求，因此在机械硬盘上快速滚动不会堆积上千个过期读取；后台预热任务以最低优先级填充缓存，并为你的任何操作让路。
- 🎬 **视频一等公民** - 关键帧缩略图、时长/编码过滤、应用内播放即点即跳，Live Photo（HEIC+MOV）处处作为一个整体。
- ♻️ **值得信赖的去重** - 按浪费空间排序分组，2-4 张并排**同步缩放**对比（在每个副本上检视同一片像素），键盘优先审阅，自动保留规则可逐张覆盖。每次删除都在最后一毫秒于磁盘上复核，并进入回收站。
- ☑️ **清理上千个重复组，不必点上千次** - 明确的复选框（点击行绝不会直接标记删除）、按规则全选、Gmail 式 Shift+点击范围选择、`Ctrl+A` / `Ctrl+D` / `Del` 快捷键，批量删除作为任务运行，带实时进度和停止按钮。重复组还会在扫描**进行中**逐步显示。
- 🗂️ **按你的方式归整** - 任何文件夹都能当库（每盘一个，名字随意），目录结构和文件名都是**模板**（`{YYYY}\{YYYY}-{MM}`、`{YYYYMMDD}_{hhmmss}_{hash4}`、`{camera}`…），强制 dry-run，同盘移动为原子重命名，每批操作有日志且可撤销。
- 📁 **保留你手工整理的分类** - `{relpath}`、`{folder}`、`{name}` 令牌（附一键预设）把分散的磁盘合并入库，同时**不会**压平你多年整理的目录结构；再次运行归整不会移动任何文件。
- 🌥️ **云端安全** - OneDrive/iCloud 占位文件会被索引但**绝不触发下载**：TidyMedia 不会悄悄从云端拉下几十 GB。
- 🕐 **时区感知** - 首次运行时选择时区；日期过滤和照片库文件名都遵循它。
- 🌍 内置 **English · Tiếng Việt · 中文**。
- 🚚 **索引可随身带走** - 导出会把数据库（连同缩略图缓存）写成一份自足的副本放进你选的文件夹；导入则在下次启动时换入，并保留一代 `.bak`。在开发版与安装版之间迁移只需复制一个文件夹，无需重新扫描。
- 📦 **小巧的原生应用** - Tauri 2 + Rust，不用 Electron。提供 NSIS 安装包、MSI，以及数据随身放在 exe 旁的便携版 ZIP。

## 🚀 快速开始

1. 从 [Releases](https://github.com/tienrocker/tidymedia/releases) 下载最新的 **setup .exe**、**.msi** 或**便携版 .zip**。
2. Windows 10/11 x64。缺少 WebView2 会自动安装；无需其他运行时。
3. 首次运行：选择**语言**和**时区** → 点击 **+ 添加**，选一个文件夹或整块磁盘（`D:\`）→ 扫描的同时即可浏览搜索。

> HEIC/AVIF 缩略图与全部视频功能使用内置的 `ffmpeg`/`ffprobe`（来自 [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) 的 GPLv3 构建，构建时锁定版本并校验 SHA256；许可证随附于 `binaries/ffmpeg-LICENSE.txt`）。
> 首次安装会出现 SmartScreen 警告（暂未购买 Authenticode 证书）。

## 🛡️ 数据安全是设计出来的

每条破坏性代码路径都必须通过的五条铁律 - 是硬约束，不是建议：

1. 没有经 **BLAKE3 校验**的存活副本，绝不删除任何文件 - 快速哈希只用来*筛选*候选，永远不是删除依据。
2. 删除/覆盖前一刻重新核对 size+mtime（TOCTOU 防护）- 不一致立即中止。
3. 同卷 ⇒ **原子重命名**；跨卷 ⇒ 复制 → flush → 校验 → 全部完成才删除源文件。
4. 所有删除都进**回收站**（隔离文件夹作后备）- 绝不直接硬删除。
5. 所有破坏性操作都有**日志**、先 dry-run 预览、且可撤销。

## 🗺️ 路线图

| 里程碑 | 范围 | 状态 |
|---|---|---|
| M1 | 整盘索引、即时搜索、虚拟化浏览 | ✅ |
| M2 | 缩略图网格、灯箱 + EXIF、HEIC、元数据任务 | ✅ |
| M3 | 视频：关键帧缩略图、应用内播放、Live Photo 配对、内置 ffmpeg | ✅ |
| M4 | 精确去重：分层哈希、2-4 张并排对比 UI、复选框批量选择、键盘优先审阅、可取消的批量删除 | ✅ |
| M5 | 归整：自选库文件夹、模板命名（可保留既有目录结构）、原子移动、撤销日志 | ✅ |
| M6 | 经 WPD/MTP 导入 iPhone / SD 卡，增量记住"已导入" | 🔜 |
| M7 | 感知相似度（同一张照片被重新压缩／缩放）、手动相册 + 标签、存储分析 | ⏳ |
| M8 | NTFS MFT/USN 极速扫描 - 整盘重扫只需数秒 | ⏳ |
| M9 | Places：GPS 索引、离线世界地图、按地点聚合照片、缩放即筛选图库 | ⏳ |
| M10 | Trips：按时间间隔 + GPS 距离自动归拢一次旅行（纯算法，无需 AI） | ⏳ |
| — | People / 人脸归类：评估中 - 需要 ~50-100 MB 的 ONNX 模型和一次全库 CPU 扫描，暂不承诺时间 | 🔬 |

> M7 为什么必须用感知哈希，基于真实 34k 文件图库的实测：同一张照片的三种导出 -
> `icloud\IMG_1463.JPEG`（3024²，1.90 MB）、`iphone\2018_08_02_…JPG`（3024²，0.79 MB）以及缩放后的
> `anh\2019\IMG_1463.JPEG`（1772²，0.87 MB）- **确实是同一张图**（PSNR 42.6 与 38.7 dB），但
> **没有任何两个字节相同**，所以精确去重放过它们是正确的。按文件名匹配也不是捷径：在两个文件夹间抽样
> 8 组同名 `IMG_####`，4 组是同一张照片（32-40 dB），4 组是毫不相干的照片（9-11 dB）。
>
> 只看感知距离同样不够，而这才是更危险的一半。同一场景的一串连拍在 64 位哈希下几乎完全一致 - 在
> 8×8 灰度网格上，换了姿势的人只占一两格 - 于是 25 张真正不同的照片落进了同一个「相似」组。没有任何
> 阈值能分开它们：收紧阈值会先丢掉真正的重新压缩版本，连拍却依然黏在一起。拍摄时间却能精确分开：那
> 25 张带有 25 个不同的 EXIF 时间戳，而 `IMG_1463` 的四个副本都精确到毫秒地带着 `18:02:24.802`。
> 两个文件只有出自同一次快门才可能是同一张照片，因此现在只要双方都有拍摄时间，就必须完全一致才会归
> 入同组。在该图库的 21.5k 张照片上，最大的组从 138 个文件降到 5 个，3,084 个文件退出了删除候选列表 -
> 同时每一组真正的重复，包括一个被剥掉 EXIF 的导出版本，都完好保留。
>
> 但该图库里另有 2,081 张照片完全没有 EXIF（被通讯 App 剥掉），这道关卡救不了它们 - 对它们来说哈希
> 是唯一的依据，而 64 位版本只剩 3 位余量。因此哈希升到 256 位。它**不能**把连拍分开（实测仍只差
> 0-8 位：即便在 16×16 网格上，移动的主体也只占少数格 - 那是拍摄时间关卡的职责）。它带来的是全面更
> 优的取舍：以 9,456 组可证明同一次快门的照片为基准，在同等漏判率下，更宽的哈希误并率只有约一半 -
> 11.4 % 对 20.6 % - 而且对整个图库聚类仍只需 272 毫秒。

## ⚙️ 性能

在 200k 文件的合成语料（由随附的 `devtool gen-tree` 生成）与真实的 218k 文件硬盘上实测：

| 操作 | 结果 |
|---|---|
| 冷扫描，200k 文件 | ~12-15 s |
| 真实硬盘，218k 文件 | ~10 s |
| 输入即搜索（FTS5 trigram） | 20-40 ms |
| 拉取一窗口结果 | < 1 ms |
| 扫描期间的 UI | 滚动、搜索一切正常 |

**技术栈：** Tauri 2 + Rust · React 18 + Vite + TanStack Virtual · SQLite（WAL、FTS5 trigram、单写入线程）

## 🛠️ 从源码构建

前置要求：Rust（stable，MSVC）、Node.js 20+。

```powershell
npm install
npm run tauri dev      # 开发（隔离的 dev 数据目录 - 不会碰真实安装）
npm run tauri build    # 发布构建：NSIS + MSI 安装包
cargo test --workspace # 无 GUI 的核心测试
```

生成测试语料 / 跑基准：

```powershell
cargo run -p devtool --release -- gen-tree --root D:\.testdata --files 200000 --dupe-sets 500
cargo run -p devtool --release -- bench-scan --root <path> --db <dir>
```

## 🌍 国际化 & 贡献

所有 UI 字符串位于 `src/locales/{en,vi,zh}.json`（react-i18next）。新字符串必须同时提交三个文件 - 欢迎添加新语言的 PR，以及附带复现步骤的 bug 报告。

## 📄 许可证

[MIT](LICENSE) © [tienrocker](https://github.com/tienrocker)
