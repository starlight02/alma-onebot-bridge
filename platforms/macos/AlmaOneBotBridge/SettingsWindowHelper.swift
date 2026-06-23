import SwiftUI
import AppKit

extension Notification.Name {
    static let almaOpenSettingsRequested = Notification.Name("almaOpenSettingsRequested")
}

/// macOS 26 的 openSettings() 在 MenuBarExtra + .accessory 模式下
/// 需要先激活 App 才能正常弹出。macOS 27 已修复，但此 helper 两版本兼容。
enum SettingsWindowHelper {
    static func open(using openSettings: OpenSettingsAction) {
        // If a settings window is already open, just bring it to front
        for window in NSApp.windows {
            let name = String(describing: window.frameAutosaveName)
            if name.contains("Settings") || window.title.contains("Alma") {
                window.makeKeyAndOrderFront(nil)
                NSApp.activate(ignoringOtherApps: true)
                return
            }
        }

        NSApp.activate(ignoringOtherApps: true)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
            openSettings()
        }
    }
}

struct SettingsRequestObserver: View {
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .onReceive(NotificationCenter.default.publisher(
                for: .almaOpenSettingsRequested
            )) { _ in
                SettingsWindowHelper.open(using: openSettings)
            }
    }
}
