import SwiftUI
import Combine
import AppKit

@main
struct AlmaOneBotBridgeApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @StateObject private var configManager: ConfigManager

    init() {
        let manager = ConfigManager()
        _configManager = StateObject(wrappedValue: manager)
        AppDelegate.configManager = manager
    }

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
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView()
                .environmentObject(configManager)
        }
        .windowStyle(.hiddenTitleBar)
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {
    static var configManager: ConfigManager?
    private var isTerminating = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        DispatchQueue.main.async {
            Self.configManager?.startBridgeIfNeeded()
        }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !isTerminating else { return .terminateLater }
        isTerminating = true

        guard let configManager = Self.configManager else {
            return .terminateNow
        }

        configManager.stopBridgeForQuit {
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    func applicationShouldTerminateAfterLastWindowClosed(
        _ sender: NSApplication
    ) -> Bool { false }
}
