# fastPaste V2.0

小而稳、零外部依赖的跨平台剪贴板效率层。对标 macOS 上的 Paste 工具（Apple App Store Story: https://apps.apple.com/cn/mac/story/id1655204803 的交互范式：居中玻璃拟态覆盖层 + 时间流卡片 + 即输即搜），按《fastPaste 系统需求说明书 V2.0》完整实现。

## 硬约束（V2.0）

| 约束 | 落实 |
|---|---|
| 安装包 ≤ 20 MB | Tauri 2.0 + 系统 WebView（复用 WebView2 / WKWebView，不打包 Chromium）；依赖刻意精简（无 ORM/数据库/重型 UI 库） |
| 不依赖任何数据库引擎 | 存储为自定义文件格式：`index.bin`（索引快照）+ `index.wal`（变更日志）+ `items/*.fpitem`（内容分片）+ `blobs/`（图片原始数据与缩略图）。无 SQLite/DuckDB/LevelDB |
| 本地优先，无后台网络 | 编译裁剪网络能力；前端不引用任何 CDN，资源全部本地打包（`withGlobalTauri`，前端零构建） |
| 单实例 | 端口探测（127.0.0.1:46371），二次启动唤起已有实例面板 |

## 目录结构

```
fastPaste/
├── src-tauri/
│   ├── Cargo.toml / tauri.conf.json / build.rs
│   ├── src/
│   │   ├── main.rs               # 入口：单实例、托盘、全局热键、面板窗口
│   │   ├── lib.rs
│   │   ├── models.rs             # ClipMeta/ClipPayload、FNV-1a 哈希、HTML→纯文本
│   │   ├── tokenizer.rs          # 中英文分词：英文词干 + 中文 bigram + 单字 unigram（FR-Q02）
│   │   ├── index.rs              # 内存倒排索引、前缀匹配、三级排序（FR-Q01/Q04/Q07）
│   │   ├── store.rs              # 文件存储：WAL 重放、原子写入、保留策略、导入导出（无 DB）
│   │   ├── sensitive.rs          # 敏感内容内存检测（FR-P03/NFR-S03）
│   │   ├── config.rs             # 配置与排除应用（FR-P01/B03）
│   │   ├── monitor.rs            # 串行剪贴板线程：30ms 延迟、退避重试、失败挂起（FR-S01~S07）
│   │   ├── clipboard/win.rs      # AddClipboardFormatListener / WM_CLIPBOARDUPDATE（FR-S01/S02/S04/S05）
│   │   ├── clipboard/mac.rs      # changeCount 轮询 500/1000ms、canReadObject 单格式读取（FR-S08）
│   │   ├── paste.rs              # Ctrl+V / ⌘V 注入；无权限降级为仅复制（FR-K03/K08）
│   │   ├── ocr/mod.rs            # OCR 引擎接口；macOS Vision / Windows.Media.Ocr 按 feature 启用（FR-Q03）
│   │   └── commands.rs           # Tauri IPC 命令（搜索/粘贴/置顶/删除/导入导出…）
│   └── tests/functional_e2e.rs   # 功能测试（对应说明书第 7 章验收标准）
├── src-tauri/icons/              # 托盘与安装包图标全套：32x32.png（托盘）、128x128.png、
│                                 # 128x128@2x.png、icon.ico（Win 多分辨率）、icon.icns（mac 多尺寸）
├── frontend/
│   ├── index.html                # Paste 风格面板（居中覆盖层、时间分组卡片、搜索）
│   ├── app.js                    # 键盘导航、Paste Stack、右键菜单、设置页
│   ├── tokenizer.js              # 与 Rust 一致的分词实现（前端测试复用）
│   └── tests/tokenizer.test.js   # node 单元测试
└── README.md
```

## 需求映射（摘要）

- **FR-H01~H08** 采集/无限历史/元数据/哈希去重/类型筛选/卡片/置顶删除/持久化 → `monitor.rs` + `store.rs` + `frontend/app.js`
- **FR-S01~S07** Windows 原生观察者 + 30ms 延迟 + 单格式读取 + 4 次退避（50/120/250/500ms）+ 瞬态过滤 + 干净写回 + 单串行线程 + 失败挂起降级 → `clipboard/win.rs`、`monitor.rs`（退避计划有单测断言）
- **FR-S08/S09** macOS 轮询 500ms/低功耗 1000ms；感知粘贴模式默认开启 → `clipboard/mac.rs`、`config.rs`
- **FR-Q01~Q07** 内存倒排索引搜索（10k 条 ≤150ms 有性能测试）、中英混合、bigram、OCR、三级排序高亮、来源应用筛选、索引与内容解耦 → `index.rs`、`ocr/mod.rs`
- **FR-K01~K08** 全局热键（冲突拒绝注册）、焦点自动入搜索框、↑↓/Enter/Esc、粘贴后收起/保留开关、双击/右键、覆盖层布局、淡入淡出、无权限降级 → `main.rs`、`paste.rs`、`frontend/*`
- **FR-A01~A05** 纯文本粘贴（Shift+Enter）、Paste Stack（⌘/Ctrl 多选带序号）、顺序调整（多选后重新点选）、浏览器打开、图片禁用纯文本提示 → `commands.rs`、`app.js`
- **FR-P01~P07 / NFR-S01~S05** 排除应用（入口即丢弃）、敏感内容不入盘、暂停开关、本地存储、清空二次确认、日志无明文 → `config.rs`、`sensitive.rs`、`store.rs`
- **NFR-R01~R04** 监听线程异常自动恢复（补偿读取在 `MonitorCmd::Probe`）、原子写入、72h 无泄漏设计（无全局句柄缓存）、与 WPS/Office 共存的时序规避（FR-S01~S06）
- **NFR-C01~C04** Win10 21H2+/Win11（x64/ARM64）、macOS 13+（Intel/Apple Silicon）、与 Win+V 共存（仅监听，不抢占剪贴板内容）、通用剪贴板兼容（mac 端标准 NSPasteboard 读取）

## 构建与运行

前置：Rust 工具链、Tauri 2 系统依赖（Windows: WebView2；macOS: Xcode CLT + 几个 framework，Tauri 官方文档）。

```bash
cd src-tauri
cargo build --release          # 开发调试运行
cargo tauri build              # 产出 MSI / DMG（安装包 ≤20MB）

# 功能测试（对应说明书第 7 章验收项）
cargo test                     # 全部功能测试
cargo test --release           # 含 10k 条搜索性能验收（≤150ms）

# 前端分词单元测试
node ../frontend/tests/tokenizer.test.js
```

macOS 首次运行：系统设置 → 隐私与安全性 → 辅助功能 → 授权 fastPaste（否则自动降级为“仅复制到剪贴板”）。macOS 15.4+ 若出现系统剪贴板隐私提示，可在设置中确认“感知粘贴模式”。

## OCR（可选 feature，控制体积）

```bash
cargo build --release --features ocr-mac   # macOS Vision（VNRecognizeTextRequest .accurate 级别）
cargo build --release --features ocr-win   # Windows.Media.Ocr（WinRT，系统语言包）
```

Windows OCR 完整实现（`ocr/mod.rs`）：`image` 解码剪贴板位图 → PNG → `InMemoryRandomAccessStream` →
`BitmapDecoder` → `SoftwareBitmap`（转 Gray8 以获得最宽语言支持）→
`OcrEngine::TryCreateFromUserProfileLanguages` → `RecognizeAsync`，识别文本回填索引后图片即可被搜索
（验收：复制含中英文截图 5 秒内可搜索命中）。

默认构建不带 OCR 引擎（Noop），图片条目照常采集与缩略图展示，仅文字不可被搜索——满足 20MB 体积基线。
