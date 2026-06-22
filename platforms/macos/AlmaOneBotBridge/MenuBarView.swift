import SwiftUI
import AppKit

struct MenuBarView: View {
    @EnvironmentObject var configManager: ConfigManager
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        Button {
        } label: {
            Text("● \(configManager.statusText)")
                .font(.body.weight(.medium))
                .foregroundStyle(statusColor)
        }

        Divider()

        Button {
            configManager.startBridge()
        } label: {
            Label("启动", systemImage: "play.fill")
        }
        .disabled(configManager.isBridgeRunning || configManager.isOperationInProgress)

        Button {
            configManager.stopBridge()
        } label: {
            Label("停止", systemImage: "stop.fill")
        }
        .disabled(!configManager.isBridgeRunning || configManager.isOperationInProgress)

        Button {
            configManager.restartBridge()
        } label: {
            Label("重启", systemImage: "arrow.clockwise")
        }
        .disabled(configManager.isOperationInProgress)

        Divider()

        Button {
            SettingsWindowHelper.open(using: openSettings)
        } label: {
            Label("设置...", systemImage: "gearshape")
        }
        .keyboardShortcut(",", modifiers: .command)

        Button {
            NSWorkspace.shared.open(configManager.configDirectoryURL)
        } label: {
            Label("打开配置目录", systemImage: "folder")
        }

        Button {
            ensureLogFileExists()
            NSWorkspace.shared.open(configManager.logFileURL)
        } label: {
            Label("打开运行日志", systemImage: "doc.text.magnifyingglass")
        }

        if let error = configManager.lastError, !error.isEmpty {
            Divider()
            Text(error)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(3)
                .disabled(true)
        }

        Divider()

        Button("退出 Alma Bridge") {
            NSApplication.shared.terminate(nil)
        }
        .keyboardShortcut("q", modifiers: .command)
    }

    private var statusColor: Color {
        if configManager.isBridgeHealthy { return .green }
        if configManager.isBridgeRunning { return .orange }
        return .secondary
    }

    private func ensureLogFileExists() {
        if !FileManager.default.fileExists(atPath: configManager.logFileURL.path()) {
            FileManager.default.createFile(atPath: configManager.logFileURL.path(), contents: nil)
        }
    }
}
