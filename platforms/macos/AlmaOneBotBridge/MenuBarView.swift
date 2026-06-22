import SwiftUI
import AppKit

struct MenuBarView: View {
    @EnvironmentObject var configManager: ConfigManager
    @Environment(\.openSettings) private var openSettings
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            statusHeader

            Divider()

            MenuPanelButton(
                title: "启动",
                systemImage: "play.fill",
                isDisabled: configManager.isBridgeRunning || configManager.isOperationInProgress
            ) {
                configManager.startBridge()
                dismiss()
            }

            MenuPanelButton(
                title: "停止",
                systemImage: "stop.fill",
                isDisabled: !configManager.isBridgeRunning || configManager.isOperationInProgress
            ) {
                configManager.stopBridge()
                dismiss()
            }

            MenuPanelButton(
                title: "重启",
                systemImage: "arrow.clockwise",
                isDisabled: configManager.isOperationInProgress
            ) {
                configManager.restartBridge()
                dismiss()
            }

            Divider()

            MenuPanelButton(title: "设置...", systemImage: "gearshape", shortcut: "⌘,") {
                SettingsWindowHelper.open(using: openSettings)
                dismiss()
            }

            MenuPanelButton(title: "打开配置目录", systemImage: "folder") {
                NSWorkspace.shared.open(configManager.configDirectoryURL)
                dismiss()
            }

            MenuPanelButton(title: "打开运行日志", systemImage: "doc.text.magnifyingglass") {
                ensureLogFileExists()
                NSWorkspace.shared.open(configManager.logFileURL)
                dismiss()
            }

            if let error = configManager.lastError, !error.isEmpty {
                Divider()
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
            }

            Divider()

            MenuPanelButton(title: "退出 Alma Bridge", systemImage: "power", shortcut: "⌘Q") {
                NSApplication.shared.terminate(nil)
            }
        }
        .padding(8)
        .frame(width: 260)
    }

    private var statusHeader: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(statusColor)
                .frame(width: 10, height: 10)

            Text(configManager.statusText)
                .font(.body.weight(.medium))
                .foregroundStyle(statusColor)

            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .accessibilityLabel(configManager.statusText)
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

private struct MenuPanelButton: View {
    let title: String
    let systemImage: String
    var shortcut: String?
    var isDisabled = false
    let action: () -> Void

    @State private var isHovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 9) {
                Image(systemName: systemImage)
                    .font(.system(size: 13, weight: .medium))
                    .frame(width: 16)
                    .foregroundStyle(.secondary)

                Text(title)
                    .font(.body)

                Spacer(minLength: 12)

                if let shortcut {
                    Text(shortcut)
                        .font(.body)
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
            .background {
                if isHovered && !isDisabled {
                    RoundedRectangle(cornerRadius: 6)
                        .fill(Color.accentColor.opacity(0.14))
                }
            }
        }
        .buttonStyle(.plain)
        .disabled(isDisabled)
        .opacity(isDisabled ? 0.42 : 1)
        .onHover { isHovered = $0 }
    }
}
