# Alma OneBot Bridge — macOS 原生应用 V1 精简方案

> **当前状态**：此文档保留为 V1 设计记录。当前 macOS 应用已经包含菜单栏托管、
> 启动/停止/重启、健康检查、原生设置、日志入口和自动安装脚本。面向用户的最新说明
> 以 [`platforms/macos/README.md`](../../platforms/macos/README.md) 为准。

> **目标系统**：macOS 26.0+（Tahoe → Golden Gate）  
> **V1 范围**：菜单栏图标 + 单个设置页面 + 进程状态 + SIGHUP 热重载  
> **架构**：Monorepo，GUI 壳在 `platforms/macos/`  
> **版本**：V1.1 · 2026-06-22（替代 V1.0 过度设计版）

---

## 1. V1 范围

### 包含

| 功能 | 说明 |
|------|------|
| 菜单栏图标 | SF Symbol，亮/暗反映 bridge 进程状态 |
| 偏好设置 | 单个 Form 页面，3 个 Section 分组，⌘, 打开 |
| 配置持久化 | 读写 `~/.config/alma/bridge/config.toml` |
| 进程状态检测 | 读 PID 文件判断 bridge 是否运行中 |
| 保存后热重载 | 写 TOML + SIGHUP 通知 bridge 重载配置 |
| 退出 | ⌘Q 退出 GUI |

### 不包含（V2+）

- 启动/停止/重启 bridge 进程
- 健康检查 HTTP 轮询
- 自动更新
- 在线日志查看

---

## 2. 技术栈

| 组件 | 版本 | 说明 |
|------|------|------|
| Xcode | 27 Beta | 需要完整 Xcode（不是 Command Line Tools） |
| Swift | 6.0+ | 随 Xcode 27 |
| SwiftUI | macOS 26+ API | MenuBarExtra, Settings Scene |
| 部署目标 | macOS 26.0 | Tahoe 及以上 |
| Swift Package | TOMLKit 0.5.x | TOML 解析 |
| App Sandbox | **关闭** | V1 需要访问 `~/.config` 和 PID 文件 |

---

## 3. 仓库结构（Monorepo）

```
alma-onebot-bridge/
├── Cargo.toml                     ← Rust bridge（核心）
├── src/                           ← Rust 源码
├── config.toml.example
├── platforms/                     ← 平台 GUI 壳
│   └── macos/
│       ├── AlmaOneBotBridge.xcodeproj
│       └── AlmaOneBotBridge/
│           ├── App.swift
│           ├── SettingsWindowHelper.swift
│           ├── ConfigModel.swift
│           ├── ConfigManager.swift
│           ├── MenuBarView.swift
│           ├── SettingsView.swift
│           └── Assets.xcassets/
└── scripts/
    └── build-macos.sh             ← 打包脚本
```

---

## 4. 文件结构（6 个 Swift 文件）

| 文件 | 职责 | 行数估算 |
|------|------|---------|
| `App.swift` | @main 入口 + AppDelegate（防关闭窗口退出） | ~40 |
| `SettingsWindowHelper.swift` | openSettings() 跨版本兼容 | ~30 |
| `ConfigModel.swift` | 13 个配置字段 + 验证 + 拷贝/比较 | ~110 |
| `ConfigManager.swift` | TOML 读写 + PID 文件读取 + SIGHUP | ~180 |
| `MenuBarView.swift` | 菜单栏下拉：状态 + 偏好设置 + 退出 | ~40 |
| `SettingsView.swift` | 单个 Form + 3 Section + 工具栏 | ~200 |

---

## 5. 架构

```
用户点击启动台 → 启动 AlmaOneBotBridge.app
                    │
         AlmaOneBotBridgeApp（Swift 主进程）
           NSApp.activationPolicy = .accessory   ← 无 Dock 图标
                    │
           ┌────────┼────────┐
           │        │        │
     MenuBarExtra  Settings  ConfigManager
     (菜单栏图标)  Scene     (TOML + PID)
           │        │        │
     MenuBarView  SettingsView  ↕ config.toml
           │        │        ↕ bridge.pid
      [状态]    [Form 表单]   ↕ SIGHUP
      [偏好设置]  [取消/储存]
      [退出]        │
                    ↓
              alma-onebot-bridge（Rust 进程，独立运行）
              读 config.toml + 写 PID 文件 + 响应 SIGHUP
```

---

## 6. 完整代码

### 6.1 `App.swift`

```swift
import SwiftUI
import AppKit

@main
struct AlmaOneBotBridgeApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @StateObject private var configManager = ConfigManager()

    var body: some Scene {
        MenuBarExtra {
            MenuBarView()
                .environmentObject(configManager)
        } label: {
            Image(systemName: configManager.isBridgeRunning
                  ? "bolt.horizontal.circle.fill"
                  : "bolt.horizontal.circle")
                .symbolRenderingMode(.hierarchical)
        }
        .menuBarExtraStyle(.menu)

        Settings {
            SettingsView()
                .environmentObject(configManager)
        }
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
    }

    func applicationShouldTerminateAfterLastWindowClosed(
        _ sender: NSApplication
    ) -> Bool { false }
}
```

### 6.2 `SettingsWindowHelper.swift`

```swift
import SwiftUI
import AppKit

/// macOS 26 的 openSettings() 在 MenuBarExtra + .accessory 模式下
/// 需要先激活 App 才能正常弹出。macOS 27 已修复，但此 helper 两版本兼容。
enum SettingsWindowHelper {
    static func open(using openSettings: OpenSettingsAction) {
        if let existing = NSApp.windows.first(where: {
            $0.frameAutosaveName.contains("Settings") ||
            $0.title.contains("设置")
        }) {
            existing.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        NSApp.activate(ignoringOtherApps: true)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
            openSettings()
        }
    }
}
```

### 6.3 `ConfigModel.swift`

```swift
import Foundation

final class ConfigModel: ObservableObject {
    // MARK: Bridge
    @Published var bridgePort: String = "8090"

    // MARK: Alma
    @Published var almaApi: String = "http://localhost:23001"
    @Published var almaModel: String = ""
    @Published var almaTimeout: String = "120"
    @Published var almaMaxRetries: String = "2"
    @Published var almaRetryDelayMs: String = "3000"

    // MARK: OneBot
    @Published var onebotApiTimeout: String = "30"

    // MARK: Security
    @Published var accessToken: String = ""

    // MARK: Chat
    @Published var groupHistorySize: String = "30"
    @Published var thinkingMessage: String = ""
    @Published var showThinking: Bool = false

    // MARK: Paths
    @Published var peopleDir: String = ""
    @Published var dbPath: String = "bridge-state.db"

    // MARK: 验证

    var isBridgePortValid: Bool {
        Int(bridgePort).map { (1...65535).contains($0) } ?? false
    }
    var isAlmaApiValid: Bool {
        !almaApi.trimmingCharacters(in: .whitespaces).isEmpty &&
        (almaApi.hasPrefix("http://") || almaApi.hasPrefix("https://"))
    }
    var isAlmaTimeoutValid: Bool {
        Int(almaTimeout).map { $0 > 0 && $0 <= 3600 } ?? false
    }
    var isAlmaMaxRetriesValid: Bool {
        Int(almaMaxRetries).map { $0 >= 0 && $0 <= 10 } ?? false
    }
    var isGroupHistorySizeValid: Bool {
        Int(groupHistorySize).map { $0 >= 0 } ?? false
    }

    var isValid: Bool {
        isBridgePortValid && isAlmaApiValid && isAlmaTimeoutValid &&
        isAlmaMaxRetriesValid && isGroupHistorySizeValid
    }

    // MARK: 拷贝与比较

    func copy() -> ConfigModel {
        let c = ConfigModel()
        c.copyValues(from: self)
        return c
    }

    func copyValues(from o: ConfigModel) {
        bridgePort = o.bridgePort; almaApi = o.almaApi
        almaModel = o.almaModel; almaTimeout = o.almaTimeout
        almaMaxRetries = o.almaMaxRetries; almaRetryDelayMs = o.almaRetryDelayMs
        onebotApiTimeout = o.onebotApiTimeout; accessToken = o.accessToken
        groupHistorySize = o.groupHistorySize; thinkingMessage = o.thinkingMessage
        showThinking = o.showThinking; peopleDir = o.peopleDir; dbPath = o.dbPath
    }

    func isEqual(to o: ConfigModel) -> Bool {
        bridgePort == o.bridgePort && almaApi == o.almaApi &&
        almaModel == o.almaModel && almaTimeout == o.almaTimeout &&
        almaMaxRetries == o.almaMaxRetries && almaRetryDelayMs == o.almaRetryDelayMs &&
        onebotApiTimeout == o.onebotApiTimeout && accessToken == o.accessToken &&
        groupHistorySize == o.groupHistorySize && thinkingMessage == o.thinkingMessage &&
        showThinking == o.showThinking && peopleDir == o.peopleDir && dbPath == o.dbPath
    }
}
```

### 6.4 `ConfigManager.swift`

```swift
import Foundation
import TOMLKit
import os.log

private let log = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "AlmaOneBotBridge",
    category: "ConfigManager"
)

final class ConfigManager: ObservableObject {
    @Published var model = ConfigModel()
    @Published var isBridgeRunning = false
    @Published var lastError: String?
    @Published var lastSaveTime: Date?

    let configFileURL: URL
    let pidFileURL: URL

    private var statusTimer: Timer?

    init() {
        let dir = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: ".config/alma/bridge")
        configFileURL = dir.appending(path: "config.toml")
        pidFileURL = dir.appending(path: "bridge.pid")

        try? FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )

        load()
        startStatusMonitor()
    }

    deinit { statusTimer?.invalidate() }

    // MARK: 进程状态监控

    private func startStatusMonitor() {
        checkBridgeStatus()
        statusTimer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            self?.checkBridgeStatus()
        }
    }

    private func checkBridgeStatus() {
        guard let data = try? String(contentsOf: pidFileURL, encoding: .utf8),
              let pid = pid_t(data.trimmingCharacters(in: .whitespaces)) else {
            DispatchQueue.main.async { self.isBridgeRunning = false }
            return
        }
        // kill(pid, 0) 检查进程是否存在，不发送信号
        let alive = kill(pid, 0) == 0
        DispatchQueue.main.async { self.isBridgeRunning = alive }
    }

    // MARK: 加载

    func load() {
        guard FileManager.default.fileExists(atPath: configFileURL.path()) else {
            log.info("config.toml not found, using defaults")
            return
        }
        do {
            let content = try String(contentsOf: configFileURL, encoding: .utf8)
            let toml = try TOMLTable(string: content)
            applyTOML(toml)
            lastError = nil
        } catch {
            lastError = "读取配置失败：\(error.localizedDescription)"
            log.error("Failed to load config: \(error)")
        }
    }

    // MARK: 保存

    func save(from editing: ConfigModel) throws {
        guard editing.isValid else {
            throw ConfigError.validationFailed("请检查字段格式")
        }

        let tomlContent = generateTOML(from: editing)
        let dir = configFileURL.deletingLastPathComponent()
        let tempURL = dir.appending(path: ".config.toml.tmp")

        do {
            try tomlContent.write(to: tempURL, atomically: false, encoding: .utf8)
            _ = try FileManager.default.replaceItemAt(
                configFileURL, withItemAt: tempURL,
                backupItemName: nil, options: .usingNewMetadataOnly
            )
        } catch {
            try? FileManager.default.removeItem(at: tempURL)
            throw ConfigError.writeFailed(error.localizedDescription)
        }

        model.copyValues(from: editing)
        lastSaveTime = Date()
        lastError = nil

        sendSIGHUP()
    }

    // MARK: SIGHUP

    private func sendSIGHUP() {
        guard let data = try? String(contentsOf: pidFileURL, encoding: .utf8),
              let pid = pid_t(data.trimmingCharacters(in: .whitespaces)) else {
            log.info("No PID file, SIGHUP skipped")
            return
        }
        let result = kill(pid, SIGHUP)
        if result == 0 {
            log.info("Sent SIGHUP to bridge (pid=\(pid))")
        } else {
            log.warning("kill(SIGHUP) failed: errno=\(errno)")
        }
    }

    // MARK: TOML 解析

    private func applyTOML(_ t: TOMLTable) {
        if let b = t["bridge"] as? TOMLTable {
            if let v = b["port"] as? Int { model.bridgePort = "\(v)" }
        }
        if let a = t["alma"] as? TOMLTable {
            if let v = a["api"] as? String { model.almaApi = v }
            if let v = a["model"] as? String { model.almaModel = v }
            if let v = a["timeout"] as? Int { model.almaTimeout = "\(v)" }
            if let v = a["max_retries"] as? Int { model.almaMaxRetries = "\(v)" }
            if let v = a["retry_delay_ms"] as? Int { model.almaRetryDelayMs = "\(v)" }
        }
        if let o = t["onebot"] as? TOMLTable {
            if let v = o["api_timeout"] as? Int { model.onebotApiTimeout = "\(v)" }
            if let v = o["access_token"] as? String { model.accessToken = v }
        }
        if let c = t["chat"] as? TOMLTable {
            if let v = c["group_history_size"] as? Int { model.groupHistorySize = "\(v)" }
            if let v = c["thinking_message"] as? String { model.thinkingMessage = v }
            if let v = c["show_thinking"] as? Bool { model.showThinking = v }
        }
        if let d = t["database"] as? TOMLTable {
            if let v = d["path"] as? String { model.dbPath = v }
        }
        if let p = t["people"] as? TOMLTable {
            if let v = p["dir"] as? String { model.peopleDir = v }
        }
    }

    // MARK: TOML 生成

    private func generateTOML(from m: ConfigModel) -> String {
        var s = """
        # Alma OneBot Bridge 配置文件
        # 由 Alma Bridge GUI 生成

        [bridge]
        port = \(Int(m.bridgePort) ?? 8090)

        [alma]
        api = "\(esc(m.almaApi))"
        """
        if !m.almaModel.isEmpty {
            s += "\nmodel = \"\(esc(m.almaModel))\""
        }
        s += """


        timeout = \(Int(m.almaTimeout) ?? 120)
        max_retries = \(Int(m.almaMaxRetries) ?? 2)
        retry_delay_ms = \(Int(m.almaRetryDelayMs) ?? 3000)

        [onebot]
        api_timeout = \(Int(m.onebotApiTimeout) ?? 30)
        """
        if !m.accessToken.isEmpty {
            s += "\naccess_token = \"\(esc(m.accessToken))\""
        }
        s += """


        [chat]
        group_history_size = \(Int(m.groupHistorySize) ?? 30)
        """
        if !m.thinkingMessage.isEmpty {
            s += "\nthinking_message = \"\(esc(m.thinkingMessage))\""
        }
        s += "\nshow_thinking = \(m.showThinking ? "true" : "false")"

        if !m.dbPath.isEmpty {
            s += """


            [database]
            path = "\(esc(m.dbPath))"
            """
        }
        if !m.peopleDir.isEmpty {
            s += """


            [people]
            dir = "\(esc(m.peopleDir))"
            """
        }
        s += "\n"
        return s
    }

    private func esc(_ s: String) -> String {
        s.replacingOccurrences(of: "\\", with: "\\\\")
         .replacingOccurrences(of: "\"", with: "\\\"")
         .replacingOccurrences(of: "\n", with: "\\n")
         .replacingOccurrences(of: "\t", with: "\\t")
    }
}

enum ConfigError: LocalizedError {
    case validationFailed(String)
    case writeFailed(String)
    var errorDescription: String? {
        switch self {
        case .validationFailed(let m): return "配置验证失败：\(m)"
        case .writeFailed(let m): return "写入文件失败：\(m)"
        }
    }
}
```

### 6.5 `MenuBarView.swift`

```swift
import SwiftUI
import AppKit

struct MenuBarView: View {
    @EnvironmentObject var configManager: ConfigManager
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        // 状态行
        HStack(spacing: 4) {
            Circle()
                .fill(configManager.isBridgeRunning ? .green : .secondary)
                .frame(width: 8, height: 8)
            Text(configManager.isBridgeRunning ? "Bridge 运行中" : "Bridge 未运行")
        }
        .disabled(true)

        Divider()

        Button {
            SettingsWindowHelper.open(using: openSettings)
        } label: {
            Label("偏好设置...", systemImage: "gear")
        }
        .keyboardShortcut(",", modifiers: .command)

        Button {
            let dir = FileManager.default.homeDirectoryForCurrentUser
                .appending(path: ".config/alma/bridge")
            NSWorkspace.shared.open(dir)
        } label: {
            Label("打开配置目录", systemImage: "folder")
        }

        Divider()

        Button("退出 Alma Bridge") {
            NSApplication.shared.terminate(nil)
        }
        .keyboardShortcut("q", modifiers: .command)
    }
}
```

### 6.6 `SettingsView.swift`

```swift
import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct SettingsView: View {
    @EnvironmentObject var configManager: ConfigManager
    @StateObject private var editing = ConfigModel()
    @State private var showDiscardAlert = false
    @State private var saveError: String?
    @State private var showSaveError = false
    @State private var showSaveSuccess = false
    @Environment(\.dismiss) private var dismiss

    private var hasChanges: Bool {
        !editing.isEqual(to: configManager.model)
    }

    var body: some View {
        Form {
            // ── Section 1: Bridge 服务 ──
            Section("Bridge 服务") {
                LabeledContent("监听端口") {
                    TextField("8090", text: $editing.bridgePort)
                        .frame(width: 80)
                        .multilineTextAlignment(.trailing)
                }
                if !editing.isBridgePortValid && !editing.bridgePort.isEmpty {
                    Label("端口须为 1–65535", systemImage: "exclamationmark.circle")
                        .foregroundStyle(.red).font(.caption)
                }

                LabeledContent("Alma API 地址") {
                    TextField("http://localhost:23001", text: $editing.almaApi)
                        .frame(minWidth: 200)
                }
                if !editing.isAlmaApiValid && !editing.almaApi.isEmpty {
                    Label("需要 http:// 或 https://", systemImage: "exclamationmark.circle")
                        .foregroundStyle(.red).font(.caption)
                }

                LabeledContent("生成超时（秒）") {
                    TextField("120", text: $editing.almaTimeout)
                        .frame(width: 64).multilineTextAlignment(.trailing)
                }
                LabeledContent("最大重试") {
                    TextField("2", text: $editing.almaMaxRetries)
                        .frame(width: 64).multilineTextAlignment(.trailing)
                }
                LabeledContent("重试延迟（毫秒）") {
                    TextField("3000", text: $editing.almaRetryDelayMs)
                        .frame(width: 80).multilineTextAlignment(.trailing)
                }
                LabeledContent("OneBot 超时（秒）") {
                    TextField("30", text: $editing.onebotApiTimeout)
                        .frame(width: 64).multilineTextAlignment(.trailing)
                }
            }

            // ── Section 2: 对话 ──
            Section("AI 对话") {
                LabeledContent("模型覆盖") {
                    TextField("留空使用默认", text: $editing.almaModel)
                        .frame(minWidth: 200)
                }
                LabeledContent("群消息历史") {
                    TextField("30", text: $editing.groupHistorySize)
                        .frame(width: 64).multilineTextAlignment(.trailing)
                }
                if !editing.isGroupHistorySizeValid && !editing.groupHistorySize.isEmpty {
                    Label("须为 0 或正整数", systemImage: "exclamationmark.circle")
                        .foregroundStyle(.red).font(.caption)
                }
                LabeledContent("思考提示语") {
                    TextField("留空禁用", text: $editing.thinkingMessage)
                        .frame(minWidth: 160)
                }
                Toggle("发送思考过程到 QQ", isOn: $editing.showThinking)
            }

            // ── Section 3: 安全与存储 ──
            Section("安全与存储") {
                LabeledContent("Access Token") {
                    HStack(spacing: 6) {
                        SecureField("留空不启用", text: $editing.accessToken)
                            .frame(minWidth: 180)
                        if !editing.accessToken.isEmpty {
                            Button {
                                editing.accessToken = ""
                            } label: {
                                Image(systemName: "xmark.circle.fill")
                                    .foregroundStyle(.secondary)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
                if editing.accessToken.isEmpty {
                    Label("未启用鉴权", systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.orange).font(.caption)
                }

                LabeledContent("数据库路径") {
                    PathField(path: $editing.dbPath,
                              placeholder: "bridge-state.db",
                              isDirectory: false)
                }
                LabeledContent("People 目录") {
                    PathField(path: $editing.peopleDir,
                              placeholder: "~/.config/alma/people",
                              isDirectory: true)
                }
            }
        }
        .formStyle(.grouped)
        .frame(minWidth: 520, idealWidth: 580, minHeight: 500)
        .navigationTitle("Alma Bridge")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("取消") {
                    if hasChanges { showDiscardAlert = true }
                    else { dismiss() }
                }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("储存") { saveSettings() }
                    .disabled(!editing.isValid || !hasChanges)
            }
        }
        .onAppear { editing.copyValues(from: configManager.model) }
        .alert("丢弃更改？", isPresented: $showDiscardAlert) {
            Button("丢弃", role: .destructive) { dismiss() }
            Button("继续编辑", role: .cancel) {}
        }
        .alert("保存失败", isPresented: $showSaveError) {
            Button("好") {}
        } message: {
            Text(saveError ?? "未知错误")
        }
        .overlay(alignment: .bottom) {
            if showSaveSuccess {
                HStack(spacing: 6) {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                    Text("配置已保存").font(.subheadline.weight(.medium))
                }
                .padding(.horizontal, 16).padding(.vertical, 8)
                .background(.regularMaterial, in: Capsule())
                .padding(.bottom, 20)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(.spring(duration: 0.3), value: showSaveSuccess)
    }

    private func saveSettings() {
        do {
            try configManager.save(from: editing)
            withAnimation { showSaveSuccess = true }
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
                withAnimation { showSaveSuccess = false }
            }
        } catch {
            saveError = error.localizedDescription
            showSaveError = true
        }
    }
}

// ── 路径选择器组件 ──

struct PathField: View {
    @Binding var path: String
    let placeholder: String
    let isDirectory: Bool

    var body: some View {
        HStack(spacing: 6) {
            TextField(placeholder, text: $path)
                .frame(minWidth: 160)
            if !path.isEmpty {
                Button { path = "" } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
            }
            Button { pickPath() } label: {
                Image(systemName: "folder.badge.plus")
            }
            .buttonStyle(.borderless)
        }
    }

    private func pickPath() {
        let panel = NSOpenPanel()
        if isDirectory {
            panel.canChooseFiles = false
            panel.canChooseDirectories = true
            panel.canCreateDirectories = true
        } else {
            panel.canChooseFiles = true
            panel.canChooseDirectories = false
        }
        panel.prompt = "选择"
        if panel.runModal() == .OK, let url = panel.url {
            path = url.path()
        }
    }
}
```

---

## 7. Rust 侧改动

### 7.1 PID 文件写入（`src/main.rs`）

在 `main()` 函数中，`warp::serve()` 之前添加：

```rust
// ── PID 文件 ──
#[cfg(unix)]
{
    use std::io::Write;
    let pid_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config").join("alma").join("bridge");
    std::fs::create_dir_all(&pid_dir).ok();
    let pid_path = pid_dir.join("bridge.pid");
    let pid = std::process::id();
    if let Ok(mut f) = std::fs::File::create(&pid_path) {
        let _ = write!(f, "{}", pid);
        tracing::info!("PID file written: {:?} (pid={})", pid_path, pid);
    }
    // 进程退出时清理 PID 文件
    let pid_path_clone = pid_path.clone();
    let _ = ctrlc::set_handler(move || {
        std::fs::remove_file(&pid_path_clone).ok();
        std::process::exit(0);
    });
}
```

需要在 `Cargo.toml` 添加依赖：

```toml
ctrlc = { version = "3", features = ["termination"] }
```

### 7.2 SIGHUP 热重载（`src/main.rs`）

```rust
#[cfg(unix)]
{
    use tokio::signal::unix::{signal, SignalKind};
    let state_hup = state.clone();
    tokio::spawn(async move {
        let mut hup = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => { tracing::warn!("SIGHUP handler failed: {}", e); return; }
        };
        loop {
            hup.recv().await;
            tracing::info!("SIGHUP received, reloading config");
            let new_cfg = Config::from_env();
            let mut cfg = state_hup.config.write().await;
            cfg.group_history_size = new_cfg.group_history_size;
            cfg.thinking_message = new_cfg.thinking_message;
            cfg.show_thinking = new_cfg.show_thinking;
            cfg.alma_run_timeout_secs = new_cfg.alma_run_timeout_secs;
            cfg.alma_max_retries = new_cfg.alma_max_retries;
            cfg.alma_retry_delay_ms = new_cfg.alma_retry_delay_ms;
            cfg.access_token = new_cfg.access_token;
            cfg.onebot_api_timeout_secs = new_cfg.onebot_api_timeout_secs;
            tracing::info!("Config hot-reload complete");
        }
    });
}
```

### 7.3 `state.rs`：Config 改为 RwLock

```rust
pub struct AppState {
    pub config: Arc<tokio::sync::RwLock<Config>>,
    // ... 其余不变
}
```

所有 `state.config.xxx` 改为 `state.config.read().await.xxx`。

### 7.4 `config.rs`：新增 GUI 配置路径

在候选列表末尾追加：

```rust
home.join(".config").join("alma").join("bridge").join("config.toml"),
```

---

## 8. 已知坑点

| # | 问题 | 解决 |
|---|------|------|
| 1 | `openSettings()` 在 macOS 26 无响应 | SettingsWindowHelper：activate + 50ms 延迟 |
| 2 | 关闭窗口导致 App 退出 | AppDelegate 返回 `false` |
| 3 | `@State` 存 class 不触发更新 | 用 `@StateObject` |
| 4 | `Form` 已内置滚动 | 不要套 `ScrollView` |
| 5 | `NSOpenPanel` 需主线程 | Button action 默认主线程 |
| 6 | `replaceItemAt` 需同卷 | .tmp 写同目录 |
| 7 | TOMLKit 整数可能是 `Int64` | 调试时 print(type(of:)) 确认 |
| 8 | `NSApp.activate` 会收起菜单 | 菜单关闭后再 activate（Button action 自然保证） |

---

## 9. 打包脚本

`scripts/build-macos.sh`：

```bash
#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MACOS_DIR="$ROOT/platforms/macos"
TARGET="aarch64-apple-darwin"

echo "==> Building Rust binary..."
cd "$ROOT"
cargo build --release --target "$TARGET"

echo "==> Copying binary to Xcode Resources..."
RESOURCES="$MACOS_DIR/AlmaOneBotBridge/Resources"
mkdir -p "$RESOURCES"
cp "$ROOT/target/$TARGET/release/alma-onebot-bridge" "$RESOURCES/"

echo "==> Building Xcode project..."
cd "$MACOS_DIR"
xcodebuild \
    -project AlmaOneBotBridge.xcodeproj \
    -scheme AlmaOneBotBridge \
    -configuration Release \
    -derivedDataPath build/ \
    CODE_SIGN_STYLE=Automatic \
    | tail -5

APP="build/Build/Products/Release/AlmaOneBotBridge.app"
echo "==> Done: $MACOS_DIR/$APP"
```

---

## 10. 实施 Checklist

```
Phase 1: 环境准备
  □ 安装 Xcode 27 Beta
  □ cargo build --release 确认 Rust bridge 编译通过
  □ Rust 侧添加 ctrlc 依赖 + PID 文件写入
  □ Rust 侧添加 SIGHUP handler + config RwLock 改造
  □ Rust 侧 config.rs 添加 ~/.config/alma/bridge/config.toml 路径
  □ 确认 Rust bridge 启动后 ~/.config/alma/bridge/bridge.pid 存在

Phase 2: Xcode 项目
  □ 新建 macOS App (SwiftUI)，Deployment Target = macOS 26.0
  □ 关闭 App Sandbox
  □ Info.plist 添加 LSUIElement = true
  □ 添加 TOMLKit 0.5.x 依赖
  □ 删除默认 ContentView.swift
  □ 添加 Build Phase 脚本自动编译 Rust（开发阶段）

Phase 3: Swift 代码
  □ App.swift（@main + AppDelegate）
  □ SettingsWindowHelper.swift
  □ ConfigModel.swift（13 字段 + 验证）
  □ ConfigManager.swift（TOML + PID + SIGHUP + 状态监控）
  □ MenuBarView.swift（状态 + 偏好设置 + 配置目录 + 退出）
  □ SettingsView.swift（3 Section Form + PathField 组件）

Phase 4: 集成测试
  □ 启动 GUI → 菜单栏图标出现
  □ Bridge 未运行时图标为空心，运行时为实心
  □ ⌘, 打开设置窗口
  □ 修改字段 → 储存 → config.toml 内容正确
  □ 修改字段 → 取消 → 弹出"丢弃"确认
  □ 字段验证错误时"储存"按钮禁用
  □ 保存成功后底部 toast 2 秒
  □ 保存后 bridge 收到 SIGHUP 并重载配置（看日志）
  □ 关闭窗口后菜单栏图标仍在

Phase 5: 打包
  □ 运行 scripts/build-macos.sh
  □ 从 Finder 双击 .app 启动
  □ 验证菜单栏图标 + 设置窗口正常工作
```
