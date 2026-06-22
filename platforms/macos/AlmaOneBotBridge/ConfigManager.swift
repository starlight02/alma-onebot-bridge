import Foundation
import Combine
import TOMLKit
import os.log

private let log = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "AlmaOneBotBridge",
    category: "ConfigManager"
)

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
            if !didExit {
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
            if !didExit {
                kill(pid, SIGKILL)
                self.removePIDFileIfMatches(pid)
            }
            self.markStopped()
            self.startBridge()
            self.isOperationInProgress = false
        }
    }

    func stopBridgeForQuit() {
        guard let pid = currentBridgePID() else { return }
        requestStop(pid: pid)
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
        } else {
            kill(pid, SIGTERM)
        }
    }

    private func waitForExit(
        pid: pid_t,
        attempts: Int = 30,
        completion: @escaping (Bool) -> Void
    ) {
        guard attempts > 0 else {
            completion(!isPIDAlive(pid))
            return
        }
        if !isPIDAlive(pid) {
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
            Task { @MainActor in
                self?.refreshBridgeStatus()
            }
        }
    }

    private func refreshBridgeStatus() {
        let runningProcessPID = process?.isRunning == true ? process?.processIdentifier : nil
        let pidFromFile = readPIDFile()
        let pid = runningProcessPID ?? pidFromFile
        let running = pid.map(isPIDAlive) ?? false

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
        var environment = ProcessInfo.processInfo.environment
        environment["RUST_LOG"] = "info"
        environment["BRIDGE_LOG_FILE"] = logFileURL.path()
        environment["ALMA_ONEBOT_BRIDGE_MANAGED_BY"] = "macos"
        return environment
    }

    private func openLogFileForAppend() throws -> FileHandle {
        if !FileManager.default.fileExists(atPath: logFileURL.path()) {
            FileManager.default.createFile(atPath: logFileURL.path(), contents: nil)
        }
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
            Task { @MainActor in
                self?.logFileHandle?.write(data)
            }
        }
    }

    private func currentBridgePID() -> pid_t? {
        if let process, process.isRunning {
            return process.processIdentifier
        }
        if let pid = readPIDFile(), isPIDAlive(pid) {
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
        kill(pid, 0) == 0
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
