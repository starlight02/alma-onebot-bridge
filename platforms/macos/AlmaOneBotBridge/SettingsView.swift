import SwiftUI
import Combine
import AppKit
import UniformTypeIdentifiers

struct SettingsView: View {
    @EnvironmentObject var configManager: ConfigManager
    @StateObject private var editing = ConfigModel()
    @State private var showDiscardAlert = false
    @State private var saveError: String?
    @State private var showSaveError = false
    @State private var saveBanner: String?
    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    private var hasChanges: Bool {
        !editing.isEqual(to: configManager.model)
    }

    var body: some View {
        VStack(spacing: 0) {
            header

            Form {
                bridgeSection
                chatSection
                storageSection
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .frame(minWidth: 720, idealWidth: 820, minHeight: 620, idealHeight: 760)
        .liquidGlassWindowBackground(reduceTransparency: reduceTransparency)
        .background(SettingsWindowConfigurator(reduceTransparency: reduceTransparency))
        .onAppear { editing.copyValues(from: configManager.model) }
        .alert("放弃更改？", isPresented: $showDiscardAlert) {
            Button("放弃", role: .destructive) { dismiss() }
            Button("继续编辑", role: .cancel) {}
        }
        .alert("保存失败", isPresented: $showSaveError) {
            Button("好") {}
        } message: {
            Text(saveError ?? "未知错误")
        }
        .animation(.snappy(duration: 0.2), value: saveBanner)
    }

    private var header: some View {
        VStack(spacing: 12) {
            HStack(spacing: 12) {
                Text("Alma Bridge")
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(.primary)

                Spacer()

                headerActions
            }

            if let saveBanner {
                Label(saveBanner, systemImage: "checkmark.circle.fill")
                    .font(.caption)
                    .foregroundStyle(.green)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 7)
                    .liquidGlassCapsule(
                        tint: .green.opacity(0.12),
                        interactive: false,
                        reduceTransparency: reduceTransparency
                    )
                    .transition(.opacity)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 24)
        .padding(.top, 18)
        .padding(.bottom, saveBanner == nil ? 14 : 12)
        .liquidGlassHeaderBackground(reduceTransparency: reduceTransparency)
        .overlay(alignment: .bottom) {
            Divider()
                .opacity(reduceTransparency ? 1 : 0.35)
        }
    }

    @ViewBuilder
    private var headerActions: some View {
        if #available(macOS 26.0, *), !reduceTransparency {
            GlassEffectContainer(spacing: 8) {
                headerButtons
            }
        } else {
            headerButtons
        }
    }

    private var headerButtons: some View {
        HStack(spacing: 8) {
            Button("取消") {
                if hasChanges {
                    showDiscardAlert = true
                } else {
                    dismiss()
                }
            }
            .keyboardShortcut(.cancelAction)
            .controlSize(.large)
            .liquidGlassButtonStyle(reduceTransparency: reduceTransparency)

            Button("保存") { saveSettings() }
                .keyboardShortcut(.defaultAction)
                .controlSize(.large)
                .liquidGlassProminentButtonStyle(reduceTransparency: reduceTransparency)
                .disabled(!editing.isValid)
        }
    }

    private var bridgeSection: some View {
        Section("Bridge 服务") {
            LabeledContent("监听端口") {
                FormTextField(
                    text: $editing.bridgePort,
                    prompt: "8090",
                    width: 96
                )
            }
            ValidationMessage(
                isVisible: !editing.isBridgePortValid && !editing.bridgePort.isEmpty,
                text: "端口必须在 1-65535 之间。"
            )

            LabeledContent("Alma API 地址") {
                FormTextField(
                    text: $editing.almaApi,
                    prompt: "http://localhost:23001",
                    width: 280
                )
            }
            ValidationMessage(
                isVisible: !editing.isAlmaApiValid && !editing.almaApi.isEmpty,
                text: "地址必须以 http:// 或 https:// 开头。"
            )

            LabeledContent("生成超时（秒）") {
                FormTextField(
                    text: $editing.almaTimeout,
                    prompt: "120",
                    width: 96
                )
            }
            ValidationMessage(
                isVisible: !editing.isAlmaTimeoutValid && !editing.almaTimeout.isEmpty,
                text: "超时时间必须在 1-3600 秒之间。"
            )

            LabeledContent("最大重试") {
                FormTextField(
                    text: $editing.almaMaxRetries,
                    prompt: "2",
                    width: 96
                )
            }
            ValidationMessage(
                isVisible: !editing.isAlmaMaxRetriesValid && !editing.almaMaxRetries.isEmpty,
                text: "重试次数必须在 0-10 之间。"
            )

            LabeledContent("重试延迟（毫秒）") {
                FormTextField(
                    text: $editing.almaRetryDelayMs,
                    prompt: "3000",
                    width: 96
                )
            }
            ValidationMessage(
                isVisible: !editing.isAlmaRetryDelayMsValid && !editing.almaRetryDelayMs.isEmpty,
                text: "重试延迟必须在 0-600000 毫秒之间。"
            )

            LabeledContent("OneBot 超时（秒）") {
                FormTextField(
                    text: $editing.onebotApiTimeout,
                    prompt: "30",
                    width: 96
                )
            }
            ValidationMessage(
                isVisible: !editing.isOneBotApiTimeoutValid && !editing.onebotApiTimeout.isEmpty,
                text: "OneBot 超时必须在 1-600 秒之间。"
            )
        }
    }

    private var chatSection: some View {
        Section("AI 对话") {
            LabeledContent("模型覆盖") {
                FormTextField(
                    text: $editing.almaModel,
                    prompt: "留空使用默认",
                    width: 240
                )
            }

            LabeledContent("群聊历史条数") {
                FormTextField(
                    text: $editing.groupHistorySize,
                    prompt: "30",
                    width: 96
                )
            }
            ValidationMessage(
                isVisible: !editing.isGroupHistorySizeValid && !editing.groupHistorySize.isEmpty,
                text: "群聊历史条数必须是 0 或正整数。"
            )

            LabeledContent("思考提示") {
                FormTextField(
                    text: $editing.thinkingMessage,
                    prompt: "留空则禁用",
                    width: 240
                )
            }

            LabeledContent("在 QQ 中显示思考") {
                Toggle("", isOn: $editing.showThinking)
                    .labelsHidden()
                    .toggleStyle(.switch)
            }
        }
    }

    private var storageSection: some View {
        Section("安全与存储") {
            LabeledContent("访问令牌") {
                HStack(spacing: 8) {
                    SecureField("禁用", text: $editing.accessToken)
                        .labelsHidden()
                        .multilineTextAlignment(.trailing)
                        .frame(width: 240, alignment: .trailing)
                    if !editing.accessToken.isEmpty {
                        Button {
                            editing.accessToken = ""
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                    }
                }
            }
            ValidationMessage(
                isVisible: editing.accessToken.isEmpty,
                text: "认证已禁用；可信内网环境下可以接受。"
            )

            LabeledContent("数据库路径") {
                PathField(
                    path: $editing.dbPath,
                    prompt: "bridge-state.db",
                    isDirectory: false
                )
            }

            LabeledContent("People 目录") {
                PathField(
                    path: $editing.peopleDir,
                    prompt: "~/.config/alma/people",
                    isDirectory: true
                )
            }
        }
    }

    private func saveSettings() {
        guard hasChanges else {
            saveBanner = "已保存"
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.4) {
                saveBanner = nil
            }
            return
        }

        do {
            try configManager.save(from: editing)
            saveBanner = bannerText(for: configManager.lastApplyAction)
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.4) {
                saveBanner = nil
            }
        } catch {
            saveError = error.localizedDescription
            showSaveError = true
        }
    }

    private func bannerText(for action: BridgeApplyAction) -> String {
        switch action {
        case .none:
            return "已保存"
        case .hotReload:
            return configManager.isBridgeRunning ? "已保存并热重载" : "已保存"
        case .restart:
            return configManager.isBridgeRunning ? "已保存，正在重启" : "已保存"
        }
    }
}

private struct FormTextField: View {
    @Binding var text: String
    let prompt: String
    let width: CGFloat

    var body: some View {
        TextField(text: $text, prompt: Text(prompt)) {
            EmptyView()
        }
        .labelsHidden()
        .multilineTextAlignment(.trailing)
        .frame(width: width, alignment: .trailing)
    }
}

private struct SettingsWindowConfigurator: NSViewRepresentable {
    let reduceTransparency: Bool

    func makeNSView(context: Context) -> NSView {
        let view = NSView(frame: .zero)
        DispatchQueue.main.async {
            configure(window: view.window)
        }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        DispatchQueue.main.async {
            configure(window: nsView.window)
        }
    }

    private func configure(window: NSWindow?) {
        guard let window else { return }
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.styleMask.insert(.fullSizeContentView)
        window.isMovableByWindowBackground = true
        window.isOpaque = reduceTransparency
        window.backgroundColor = reduceTransparency ? .windowBackgroundColor : .clear
    }
}

private extension View {
    @ViewBuilder
    func liquidGlassWindowBackground(reduceTransparency: Bool) -> some View {
        if reduceTransparency {
            self.background(Color(nsColor: .windowBackgroundColor))
        } else if #available(macOS 26.0, *) {
            self.containerBackground(.regularMaterial, for: .window)
        } else {
            self.background(.regularMaterial)
        }
    }

    @ViewBuilder
    func liquidGlassHeaderBackground(reduceTransparency: Bool) -> some View {
        if reduceTransparency {
            self.background(Color(nsColor: .windowBackgroundColor))
        } else {
            self.background(.regularMaterial)
        }
    }

    @ViewBuilder
    func liquidGlassButtonStyle(reduceTransparency: Bool) -> some View {
        if #available(macOS 26.0, *), !reduceTransparency {
            self.buttonStyle(.glass)
        } else {
            self.buttonStyle(.bordered)
        }
    }

    @ViewBuilder
    func liquidGlassProminentButtonStyle(reduceTransparency: Bool) -> some View {
        if #available(macOS 26.0, *), !reduceTransparency {
            self.buttonStyle(.glassProminent)
        } else {
            self.buttonStyle(.borderedProminent)
        }
    }

    @ViewBuilder
    func liquidGlassCapsule(
        tint: Color?,
        interactive: Bool,
        reduceTransparency: Bool
    ) -> some View {
        if reduceTransparency {
            self
                .background(Color(nsColor: .controlBackgroundColor), in: Capsule())
                .overlay {
                    Capsule()
                        .stroke((tint ?? .primary).opacity(0.35), lineWidth: 1)
                }
        } else if #available(macOS 26.0, *) {
            self.glassEffect(.regular.tint(tint).interactive(interactive), in: Capsule())
        } else {
            self.background(.thinMaterial, in: Capsule())
        }
    }
}

private struct ValidationMessage: View {
    let isVisible: Bool
    // LocalizedStringKey so literal text is resolved through the catalog.
    let text: LocalizedStringKey

    var body: some View {
        if isVisible {
            Label(text, systemImage: "exclamationmark.circle")
                .font(.caption)
                .foregroundStyle(.orange)
        }
    }
}

private struct PathField: View {
    @Binding var path: String
    let prompt: String
    let isDirectory: Bool

    var body: some View {
        HStack(spacing: 8) {
            TextField(text: $path, prompt: Text(prompt)) {
                EmptyView()
            }
            .labelsHidden()
            .multilineTextAlignment(.trailing)
            .frame(width: 320, alignment: .trailing)

            if !path.isEmpty {
                Button {
                    path = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
            }

            Button {
                pickPath()
            } label: {
                Image(systemName: isDirectory ? "folder.badge.plus" : "doc.badge.plus")
            }
            .buttonStyle(.borderless)
            .help("选择路径")
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
