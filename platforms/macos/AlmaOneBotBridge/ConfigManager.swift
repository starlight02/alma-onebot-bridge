import Foundation
import Combine
import TOMLKit
import os.log
import Darwin
import AppKit

private let log = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "AlmaOneBotBridge",
    category: "ConfigManager"
)
private let maxBridgeLogBytes: UInt64 = 10 * 1024 * 1024
private let bridgeLogBackupCount = 3
private let projectName = "alma-onebot-bridge"
private let projectURL = "https://github.com/starlight02/alma-onebot-bridge"
private let projectAuthor = "星光の殲滅者"
private let projectAuthorURL = "https://github.com/starlight02"
private let projectLicense = "AGPL-3.0-only"
private let projectLicenseURL = "https://spdx.org/licenses/AGPL-3.0-only"

private final class LinkOpeningTextView: NSTextView {
    override func clicked(onLink link: Any, at charIndex: Int) {
        if let url = link as? URL {
            NSWorkspace.shared.open(url)
            return
        }
        if let urlString = link as? String,
           let url = URL(string: urlString) {
            NSWorkspace.shared.open(url)
            return
        }
        super.clicked(onLink: link, at: charIndex)
    }
}

enum BridgeApplyAction {
    case none
    case hotReload
    case restart
}

@MainActor
final class ConfigManager: ObservableObject {
    @Published var model = ConfigModel()
    @Published var isBridgeRunning = false
    @Published var isBridgeHealthy = false
    @Published var isOperationInProgress = false
    @Published var bridgePID: pid_t?
    @Published var statusText = "已停止"
    @Published var lastError: String?
    @Published var lastSaveTime: Date?
    @Published var lastStartTime: Date?
    @Published var lastApplyAction: BridgeApplyAction = .none

    let configDirectoryURL: URL
    let configFileURL: URL
    let pidFileURL: URL
    let logFileURL: URL

    private var process: Process?
    private var logFileHandle: FileHandle?
    private var stdoutPipe: Pipe?
    private var stderrPipe: Pipe?
    private var statusTimer: Timer?
    private var healthTask: URLSessionDataTask?

    init() {
        configDirectoryURL = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: ".config/alma/bridge")
        configFileURL = configDirectoryURL.appending(path: "config.toml")
        pidFileURL = configDirectoryURL.appending(path: "bridge.pid")
        logFileURL = configDirectoryURL.appending(path: "bridge.log")

        try? FileManager.default.createDirectory(
            at: configDirectoryURL,
            withIntermediateDirectories: true
        )

        load()
        generateDefaultIfNeeded()
        startStatusMonitor()
    }

    deinit {
        statusTimer?.invalidate()
        healthTask?.cancel()
        try? logFileHandle?.close()
    }

    // MARK: Process lifecycle

    func startBridgeIfNeeded() {
        refreshBridgeStatus()
        guard !isBridgeRunning else { return }
        startBridge()
    }

    func refreshStatus() {
        refreshBridgeStatus()
    }

    func startBridge() {
        refreshBridgeStatus()
        guard !isBridgeRunning else {
            lastError = nil
            return
        }

        guard let executableURL = bridgeExecutableURL() else {
            lastError = "未找到随 app 打包的桥接服务可执行文件，请使用 scripts/build-macos.sh 重新构建。"
            statusText = "缺少桥接服务"
            log.error("Bridge executable not found")
            return
        }

        guard ensureBridgePortAvailable() else {
            return
        }

        do {
            try FileManager.default.createDirectory(
                at: configDirectoryURL,
                withIntermediateDirectories: true
            )

            let handle = try openLogFileForAppend()
            let stdoutPipe = Pipe()
            let stderrPipe = Pipe()
            let task = Process()
            task.executableURL = executableURL
            task.currentDirectoryURL = configDirectoryURL
            task.environment = bridgeEnvironment()
            task.standardOutput = stdoutPipe
            task.standardError = stderrPipe
            task.terminationHandler = { [weak self] terminatedProcess in
                DispatchQueue.main.async {
                    self?.handleBridgeTermination(terminatedProcess)
                }
            }

            logFileHandle = handle
            self.stdoutPipe = stdoutPipe
            self.stderrPipe = stderrPipe
            pipeBridgeOutput(stdoutPipe)
            pipeBridgeOutput(stderrPipe)

            try task.run()
            process = task
            lastStartTime = Date()
            lastError = nil
            isBridgeRunning = true
            isBridgeHealthy = false
            bridgePID = task.processIdentifier
            statusText = "正在启动：端口 \(model.bridgePort)"
            log.info("Started bridge pid=\(task.processIdentifier)")
        } catch {
            stdoutPipe?.fileHandleForReading.readabilityHandler = nil
            stderrPipe?.fileHandleForReading.readabilityHandler = nil
            stdoutPipe = nil
            stderrPipe = nil
            try? logFileHandle?.close()
            logFileHandle = nil
            lastError = "启动桥接服务失败：\(error.localizedDescription)"
            statusText = "启动失败"
            log.error("Failed to start bridge: \(error)")
        }
    }

    func stopBridge() {
        guard let pid = currentBridgePID() else {
            markStopped()
            return
        }

        isOperationInProgress = true
        statusText = "正在停止..."
        requestStop(pid: pid)
        waitForExit(pid: pid) { [weak self] didExit in
            guard let self else { return }
            if !didExit, self.isManagedBridgeProcess(pid) {
                kill(pid, SIGKILL)
                self.removePIDFileIfMatches(pid)
            }
            self.markStopped()
            self.isOperationInProgress = false
        }
    }

    func restartBridge() {
        guard let pid = currentBridgePID() else {
            startBridge()
            return
        }

        isOperationInProgress = true
        statusText = "正在重启..."
        requestStop(pid: pid)
        waitForExit(pid: pid) { [weak self] didExit in
            guard let self else { return }
            if !didExit, self.isManagedBridgeProcess(pid) {
                kill(pid, SIGKILL)
                self.removePIDFileIfMatches(pid)
            }
            self.markStopped()
            self.startBridge()
            self.isOperationInProgress = false
        }
    }

    func stopBridgeForQuit(completion: @escaping () -> Void) {
        guard let pid = currentBridgePID() else {
            completion()
            return
        }
        requestStop(pid: pid)
        waitForExit(pid: pid, attempts: 15) { [weak self] didExit in
            guard let self else {
                completion()
                return
            }
            if !didExit, self.isManagedBridgeProcess(pid) {
                kill(pid, SIGKILL)
                self.removePIDFileIfMatches(pid)
            }
            self.markStopped()
            completion()
        }
    }

    private func handleBridgeTermination(_ terminatedProcess: Process) {
        guard process === terminatedProcess else { return }
        let status = terminatedProcess.terminationStatus
        process = nil
        stdoutPipe?.fileHandleForReading.readabilityHandler = nil
        stderrPipe?.fileHandleForReading.readabilityHandler = nil
        stdoutPipe = nil
        stderrPipe = nil
        try? logFileHandle?.close()
        logFileHandle = nil

        if status == 0 || status == SIGTERM {
            log.info("Bridge exited with status \(status)")
        } else {
            lastError = "桥接服务退出，状态码 \(status)。请查看 bridge.log。"
            log.warning("Bridge exited with status \(status)")
        }
        refreshBridgeStatus()
    }

    private func requestStop(pid: pid_t) {
        if let process, process.isRunning, process.processIdentifier == pid {
            process.terminate()
        } else if isManagedBridgeProcess(pid) {
            kill(pid, SIGTERM)
        } else {
            removePIDFileIfMatches(pid)
            log.warning("Skipped SIGTERM for non-bridge pid=\(pid)")
        }
    }

    private func waitForExit(
        pid: pid_t,
        attempts: Int = 30,
        completion: @escaping (Bool) -> Void
    ) {
        guard attempts > 0 else {
            completion(!isManagedBridgeProcess(pid))
            return
        }
        if !isManagedBridgeProcess(pid) {
            completion(true)
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { [weak self] in
            self?.waitForExit(pid: pid, attempts: attempts - 1, completion: completion)
        }
    }

    // MARK: Status monitor

    private func startStatusMonitor() {
        refreshBridgeStatus()
        statusTimer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.refreshBridgeStatus()
            }
        }
    }

    private func refreshBridgeStatus() {
        let runningProcessPID = process?.isRunning == true ? process?.processIdentifier : nil
        let pidFromFile = readPIDFile()
        let pid = runningProcessPID ?? pidFromFile
        let running = runningProcessPID != nil || pidFromFile.map(isManagedBridgeProcess) == true

        if !running, let stalePID = pidFromFile {
            removePIDFileIfMatches(stalePID)
        }

        isBridgeRunning = running
        bridgePID = running ? pid : nil
        if !running {
            isBridgeHealthy = false
            statusText = isOperationInProgress ? statusText : "已停止"
            healthTask?.cancel()
            return
        }

        probeHealth()
        statusText = isBridgeHealthy
            ? "运行中：端口 \(model.bridgePort)"
            : "运行中，正在检查状态..."
    }

    private func probeHealth() {
        guard let port = Int(model.bridgePort),
              let url = URL(string: "http://127.0.0.1:\(port)/health") else {
            isBridgeHealthy = false
            return
        }

        healthTask?.cancel()
        var request = URLRequest(url: url)
        request.timeoutInterval = 1
        healthTask = URLSession.shared.dataTask(with: request) { [weak self] data, response, _ in
            let statusCode = (response as? HTTPURLResponse)?.statusCode
            let body = data.flatMap { String(data: $0, encoding: .utf8) } ?? ""
            let healthy = statusCode == 200 && body.contains("alma-onebot-bridge")
            DispatchQueue.main.async {
                self?.isBridgeHealthy = healthy
                if self?.isBridgeRunning == true {
                    self?.statusText = healthy
                        ? "运行中：端口 \(self?.model.bridgePort ?? "")"
                        : "运行中，健康检查失败"
                }
            }
        }
        healthTask?.resume()
    }

    private func markStopped() {
        process = nil
        stdoutPipe?.fileHandleForReading.readabilityHandler = nil
        stderrPipe?.fileHandleForReading.readabilityHandler = nil
        stdoutPipe = nil
        stderrPipe = nil
        try? logFileHandle?.close()
        logFileHandle = nil
        isBridgeRunning = false
        isBridgeHealthy = false
        bridgePID = nil
        statusText = "已停止"
        healthTask?.cancel()
    }

    private func ensureBridgePortAvailable() -> Bool {
        guard let port = UInt16(model.bridgePort) else {
            return true
        }

        if isTCPPortAvailable(port) {
            return true
        }

        let message = "端口 \(port) 已被占用。请关闭占用该端口的应用，或在设置中修改 Bridge 端口。"
        lastError = message
        statusText = "启动失败：端口 \(port) 被占用"
        isBridgeRunning = false
        isBridgeHealthy = false
        bridgePID = nil

        appendAppLogLine("Startup blocked: port \(port) is already in use.")
        showPortOccupiedAlert(port: port, message: message)
        return false
    }

    private func isTCPPortAvailable(_ port: UInt16) -> Bool {
        if canConnectToLoopback(port) {
            return false
        }

        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }

        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        addr.sin_addr = in_addr(s_addr: INADDR_ANY.bigEndian)

        let result = withUnsafePointer(to: &addr) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockaddrPointer in
                Darwin.bind(fd, sockaddrPointer, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }

        return result == 0
    }

    private func canConnectToLoopback(_ port: UInt16) -> Bool {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }

        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        addr.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))

        let result = withUnsafePointer(to: &addr) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockaddrPointer in
                Darwin.connect(fd, sockaddrPointer, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }

        return result == 0
    }

    private func showPortOccupiedAlert(port: UInt16, message: String) {
        NSApp.activate(ignoringOtherApps: true)

        let alert = NSAlert()
        alert.alertStyle = .critical
        alert.messageText = "Alma Bridge 无法启动"
        alert.informativeText = "\(message)\n\n当前配置端口：\(port)"
        alert.addButton(withTitle: "打开设置")
        alert.addButton(withTitle: "打开运行日志")
        alert.addButton(withTitle: "好")

        let response = alert.runModal()
        switch response {
        case .alertFirstButtonReturn:
            NotificationCenter.default.post(name: .almaOpenSettingsRequested, object: nil)
        case .alertSecondButtonReturn:
            ensureLogFileExists()
            NSApp.activate(ignoringOtherApps: true)
            NSWorkspace.shared.open(logFileURL)
        default:
            break
        }
    }

    func showAboutAlert() {
        NSApp.activate(ignoringOtherApps: true)

        let clipboardInformation = aboutClipboardInformationText()
        let alert = NSAlert()
        alert.alertStyle = .informational
        alert.icon = NSApp.applicationIconImage
        alert.messageText = "\(applicationName) \(applicationVersionDisplay)"
        alert.accessoryView = aboutInformationView()
        alert.addButton(withTitle: "好")
        alert.addButton(withTitle: "复制信息")

        let response = alert.runModal()
        if response == .alertSecondButtonReturn {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(
                "\(alert.messageText)\n\n\(clipboardInformation)",
                forType: .string
            )
        }
    }

    private var applicationName: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String
            ?? Bundle.main.object(forInfoDictionaryKey: "CFBundleName") as? String
            ?? "AlmaOneBotBridge"
    }

    private var applicationVersionDisplay: String {
        let version = Bundle.main.object(
            forInfoDictionaryKey: "CFBundleShortVersionString"
        ) as? String ?? "Unknown"
        return "版本 \(version)"
    }

    private var sourceVersionDisplay: String {
        let commit = Bundle.main.object(forInfoDictionaryKey: "AlmaGitCommit") as? String
            ?? "Unknown"
        let isDirty = (
            Bundle.main.object(forInfoDictionaryKey: "AlmaGitDirty") as? String
        ) == "true"

        if isDirty, commit != "Unknown" {
            return "\(commit)（本地构建含未提交修改）"
        }
        return commit
    }

    private var aboutStatusDisplay: String {
        let portSuffix = "：端口 \(model.bridgePort)"
        if statusText.hasSuffix(portSuffix) {
            return String(statusText.dropLast(portSuffix.count))
        }
        return statusText
    }

    private func aboutInformationView() -> NSView {
        let width: CGFloat = 420
        let textView = LinkOpeningTextView(frame: NSRect(x: 0, y: 0, width: width, height: 170))
        textView.drawsBackground = false
        textView.isEditable = false
        textView.isSelectable = true
        textView.textContainerInset = .zero
        textView.textContainer?.lineFragmentPadding = 0
        textView.textContainer?.widthTracksTextView = true
        textView.textContainer?.containerSize = NSSize(
            width: width,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.textStorage?.setAttributedString(aboutInformationAttributedText())

        if let layoutManager = textView.layoutManager,
           let textContainer = textView.textContainer {
            layoutManager.ensureLayout(for: textContainer)
            let usedRect = layoutManager.usedRect(for: textContainer)
            textView.frame.size = NSSize(width: width, height: ceil(usedRect.height))
        }

        return textView
    }

    private func aboutInformationAttributedText() -> NSAttributedString {
        let text = NSMutableAttributedString()
        let paragraphStyle = NSMutableParagraphStyle()
        paragraphStyle.lineSpacing = 2
        let font = NSFont.systemFont(ofSize: NSFont.systemFontSize)
        let baseAttributes: [NSAttributedString.Key: Any] = [
            .font: font,
            .foregroundColor: NSColor.labelColor,
            .paragraphStyle: paragraphStyle
        ]

        func append(_ value: String, attributes: [NSAttributedString.Key: Any] = baseAttributes) {
            text.append(NSAttributedString(string: value, attributes: attributes))
        }

        func appendLine(_ value: String = "") {
            append("\(value)\n")
        }

        func appendLinkedLine(label: String, value: String, urlString: String) {
            append(label)
            var linkAttributes = baseAttributes
            linkAttributes[.link] = urlString
            linkAttributes[.foregroundColor] = NSColor.linkColor
            linkAttributes[.underlineStyle] = NSUnderlineStyle.single.rawValue
            append(value, attributes: linkAttributes)
            appendLine()
        }

        appendLine("提交版本：\(sourceVersionDisplay)")
        appendLinkedLine(label: "项目地址：", value: projectName, urlString: projectURL)
        appendLinkedLine(label: "作者：", value: projectAuthor, urlString: projectAuthorURL)
        appendLinkedLine(label: "开源协议：", value: projectLicense, urlString: projectLicenseURL)
        appendLine()
        appendLine("运行状态：\(aboutStatusDisplay)")
        appendLine("监听端口：\(model.bridgePort)")
        appendLine("Bridge PID：\(bridgePID.map(String.init) ?? "无")")
        append("Alma API：\(model.almaApi)")

        return text
    }

    private func aboutClipboardInformationText() -> String {
        let pidText = bridgePID.map(String.init) ?? "无"

        return """
        提交版本：\(sourceVersionDisplay)
        运行状态：\(aboutStatusDisplay)
        监听端口：\(model.bridgePort)
        Bridge PID：\(pidText)
        Alma API：\(model.almaApi)
        """
    }

    // MARK: Load

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
            lastError = "加载配置失败：\(error.localizedDescription)"
            log.error("Failed to load config: \(error)")
        }
    }

    // MARK: Auto-generate default config

    private func generateDefaultIfNeeded() {
        guard !FileManager.default.fileExists(atPath: configFileURL.path()) else { return }
        do {
            let content = generateTOML(from: model)
            try content.write(to: configFileURL, atomically: true, encoding: .utf8)
            log.info("Generated default config.toml at \(self.configFileURL.path())")
        } catch {
            log.warning("Failed to generate default config.toml: \(error)")
        }
    }

    // MARK: Save

    func save(from editing: ConfigModel) throws {
        guard editing.isValid else {
            throw ConfigError.validationFailed("请检查配置项格式。")
        }

        let previous = model.copy()
        let tomlContent = generateTOML(from: editing)
        let dir = configFileURL.deletingLastPathComponent()
        let tempURL = dir.appending(path: ".config.toml.tmp")

        do {
            try tomlContent.write(to: tempURL, atomically: false, encoding: .utf8)
            _ = try FileManager.default.replaceItemAt(
                configFileURL,
                withItemAt: tempURL,
                backupItemName: nil,
                options: .usingNewMetadataOnly
            )
        } catch {
            try? FileManager.default.removeItem(at: tempURL)
            throw ConfigError.writeFailed(error.localizedDescription)
        }

        model.copyValues(from: editing)
        lastSaveTime = Date()
        lastError = nil

        applySavedConfig(previous: previous, current: editing)
    }

    private func applySavedConfig(previous: ConfigModel, current: ConfigModel) {
        let requiresRestart = previous.requiresBridgeRestart(to: current)
        if requiresRestart {
            lastApplyAction = .restart
            if isBridgeRunning {
                restartBridge()
            }
            return
        }

        lastApplyAction = .hotReload
        if isBridgeRunning {
            sendSIGHUP()
        }
    }

    // MARK: SIGHUP

    private func sendSIGHUP() {
        guard let pid = currentBridgePID() else {
            log.info("No bridge PID, SIGHUP skipped")
            return
        }
        let result = kill(pid, SIGHUP)
        if result == 0 {
            log.info("Sent SIGHUP to bridge pid=\(pid)")
        } else {
            lastError = "重载桥接服务配置失败：errno \(errno)"
            log.warning("kill(SIGHUP) failed: errno=\(errno)")
        }
    }

    // MARK: Paths and process helpers

    private func bridgeExecutableURL() -> URL? {
        let fileManager = FileManager.default
        let resourceURL = Bundle.main.resourceURL?.appending(path: "alma-onebot-bridge")
        if let resourceURL, fileManager.isExecutableFile(atPath: resourceURL.path()) {
            return resourceURL
        }

        for candidate in developmentBridgeCandidates() {
            if fileManager.isExecutableFile(atPath: candidate.path()) {
                return candidate
            }
        }
        return nil
    }

    private func developmentBridgeCandidates() -> [URL] {
        var candidates: [URL] = []
        var cursor = Bundle.main.bundleURL
        for _ in 0..<10 {
            let target = cursor.appending(path: "target")
            candidates.append(target.appending(path: "debug/alma-onebot-bridge"))
            candidates.append(target.appending(path: "release/alma-onebot-bridge"))
            candidates.append(target.appending(path: "aarch64-apple-darwin/release/alma-onebot-bridge"))
            candidates.append(target.appending(path: "aarch64-apple-darwin/debug/alma-onebot-bridge"))
            cursor.deleteLastPathComponent()
        }
        return candidates
    }

    private func bridgeEnvironment() -> [String: String] {
        let parentEnvironment = ProcessInfo.processInfo.environment
        var environment: [String: String] = [:]
        for key in ["HOME", "PATH", "TMPDIR", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "LC_CTYPE"] {
            environment[key] = parentEnvironment[key]
        }
        environment["RUST_LOG"] = "info"
        environment["BRIDGE_LOG_FILE"] = logFileURL.path()
        environment["ALMA_ONEBOT_BRIDGE_MANAGED_BY"] = "macos"
        return environment
    }

    private func openLogFileForAppend() throws -> FileHandle {
        try FileManager.default.createDirectory(
            at: configDirectoryURL,
            withIntermediateDirectories: true
        )
        rotateLogFileIfNeeded()
        ensureLogFileExists()

        let handle = try FileHandle(forWritingTo: logFileURL)
        handle.seekToEndOfFile()
        let header = "\n--- Alma OneBot Bridge launch \(Date()) ---\n"
        if let data = header.data(using: .utf8) {
            handle.write(data)
        }
        return handle
    }

    private func pipeBridgeOutput(_ pipe: Pipe) {
        pipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty else { return }
            Task { @MainActor [weak self] in
                self?.logFileHandle?.write(data)
            }
        }
    }

    func ensureLogFileExists() {
        try? FileManager.default.createDirectory(
            at: configDirectoryURL,
            withIntermediateDirectories: true
        )
        rotateLogFileIfNeeded()
        if !FileManager.default.fileExists(atPath: logFileURL.path()) {
            _ = FileManager.default.createFile(atPath: logFileURL.path(), contents: nil)
        }
    }

    private func appendAppLogLine(_ message: String) {
        ensureLogFileExists()
        guard let handle = try? FileHandle(forWritingTo: logFileURL) else {
            log.warning("Failed to open bridge log for app diagnostic")
            return
        }
        defer { try? handle.close() }

        handle.seekToEndOfFile()
        let timestamp = ISO8601DateFormatter().string(from: Date())
        let line = "\(timestamp) [macOS app] \(message)\n"
        if let data = line.data(using: .utf8) {
            handle.write(data)
        }
    }

    private func rotateLogFileIfNeeded() {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: logFileURL.path()),
              let fileSize = attributes[.size] as? UInt64,
              fileSize > maxBridgeLogBytes else {
            return
        }

        let oldest = rotatedLogFileURL(bridgeLogBackupCount)
        if FileManager.default.fileExists(atPath: oldest.path()) {
            try? FileManager.default.removeItem(at: oldest)
        }

        if bridgeLogBackupCount > 1 {
            for index in stride(from: bridgeLogBackupCount - 1, through: 1, by: -1) {
                let from = rotatedLogFileURL(index)
                guard FileManager.default.fileExists(atPath: from.path()) else { continue }
                try? FileManager.default.moveItem(at: from, to: rotatedLogFileURL(index + 1))
            }
        }

        try? FileManager.default.moveItem(at: logFileURL, to: rotatedLogFileURL(1))
    }

    private func rotatedLogFileURL(_ index: Int) -> URL {
        logFileURL.deletingLastPathComponent()
            .appending(path: "\(logFileURL.lastPathComponent).\(index)")
    }

    private func currentBridgePID() -> pid_t? {
        if let process, process.isRunning {
            return process.processIdentifier
        }
        if let pid = readPIDFile(), isManagedBridgeProcess(pid) {
            return pid
        }
        return nil
    }

    private func readPIDFile() -> pid_t? {
        guard let data = try? String(contentsOf: pidFileURL, encoding: .utf8) else {
            return nil
        }
        return pid_t(data.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    private func isPIDAlive(_ pid: pid_t) -> Bool {
        if kill(pid, 0) == 0 {
            return true
        }
        return errno == EPERM
    }

    private func isManagedBridgeProcess(_ pid: pid_t) -> Bool {
        guard isPIDAlive(pid), let executablePath = executablePath(for: pid) else {
            return false
        }
        return URL(fileURLWithPath: executablePath).lastPathComponent == "alma-onebot-bridge"
    }

    private func executablePath(for pid: pid_t) -> String? {
        var buffer = [CChar](repeating: 0, count: 4096)
        let length = proc_pidpath(pid, &buffer, UInt32(buffer.count))
        guard length > 0 else { return nil }
        return String(cString: buffer)
    }

    private func removePIDFileIfMatches(_ pid: pid_t) {
        guard readPIDFile() == pid else { return }
        try? FileManager.default.removeItem(at: pidFileURL)
    }

    // MARK: TOML parsing

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

    // MARK: TOML generation

    private func generateTOML(from m: ConfigModel) -> String {
        var lines: [String] = []
        lines.append("# Alma OneBot Bridge config")
        lines.append("# Generated by Alma Bridge GUI")
        lines.append("")
        lines.append("[bridge]")
        lines.append("port = \(Int(m.bridgePort) ?? 8090)")
        lines.append("")
        lines.append("[alma]")
        lines.append("api = \"\(esc(m.almaApi))\"")
        if !m.almaModel.isEmpty {
            lines.append("model = \"\(esc(m.almaModel))\"")
        }
        lines.append("timeout = \(Int(m.almaTimeout) ?? 120)")
        lines.append("max_retries = \(Int(m.almaMaxRetries) ?? 2)")
        lines.append("retry_delay_ms = \(Int(m.almaRetryDelayMs) ?? 3000)")
        lines.append("")
        lines.append("[onebot]")
        lines.append("api_timeout = \(Int(m.onebotApiTimeout) ?? 30)")
        if !m.accessToken.isEmpty {
            lines.append("access_token = \"\(esc(m.accessToken))\"")
        }
        lines.append("")
        lines.append("[chat]")
        lines.append("group_history_size = \(Int(m.groupHistorySize) ?? 30)")
        if !m.thinkingMessage.isEmpty {
            lines.append("thinking_message = \"\(esc(m.thinkingMessage))\"")
        }
        lines.append("show_thinking = \(m.showThinking ? "true" : "false")")

        if !m.dbPath.isEmpty {
            lines.append("")
            lines.append("[database]")
            lines.append("path = \"\(esc(m.dbPath))\"")
        }
        if !m.peopleDir.isEmpty {
            lines.append("")
            lines.append("[people]")
            lines.append("dir = \"\(esc(m.peopleDir))\"")
        }
        lines.append("")
        return lines.joined(separator: "\n")
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
        case .validationFailed(let message):
            return "校验失败：\(message)"
        case .writeFailed(let message):
            return "写入失败：\(message)"
        }
    }
}
