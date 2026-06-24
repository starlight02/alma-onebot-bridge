import Foundation
import Combine

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

    // MARK: Validation

    var isBridgePortValid: Bool {
        Int(bridgePort).map { (1...65535).contains($0) } ?? false
    }
    var isAlmaApiValid: Bool {
        !almaApi.trimmingCharacters(in: .whitespaces).isEmpty
        && (almaApi.hasPrefix("http://") || almaApi.hasPrefix("https://"))
    }
    var isAlmaTimeoutValid: Bool {
        Int(almaTimeout).map { $0 > 0 && $0 <= 3600 } ?? false
    }
    var isAlmaMaxRetriesValid: Bool {
        Int(almaMaxRetries).map { $0 >= 0 && $0 <= 10 } ?? false
    }
    var isAlmaRetryDelayMsValid: Bool {
        Int(almaRetryDelayMs).map { $0 >= 0 && $0 <= 600_000 } ?? false
    }
    var isOneBotApiTimeoutValid: Bool {
        Int(onebotApiTimeout).map { $0 > 0 && $0 <= 600 } ?? false
    }
    var isGroupHistorySizeValid: Bool {
        Int(groupHistorySize).map { $0 >= 0 } ?? false
    }

    var isValid: Bool {
        isBridgePortValid && isAlmaApiValid && isAlmaTimeoutValid
        && isAlmaMaxRetriesValid && isAlmaRetryDelayMsValid
        && isOneBotApiTimeoutValid && isGroupHistorySizeValid
    }

    // MARK: Copy & Compare

    func copy() -> ConfigModel {
        let c = ConfigModel()
        c.copyValues(from: self)
        return c
    }

    func copyValues(from o: ConfigModel) {
        bridgePort = o.bridgePort
        almaApi = o.almaApi
        almaModel = o.almaModel
        almaTimeout = o.almaTimeout
        almaMaxRetries = o.almaMaxRetries
        almaRetryDelayMs = o.almaRetryDelayMs
        onebotApiTimeout = o.onebotApiTimeout
        accessToken = o.accessToken
        groupHistorySize = o.groupHistorySize
        thinkingMessage = o.thinkingMessage
        showThinking = o.showThinking
        peopleDir = o.peopleDir
        dbPath = o.dbPath
    }

    func isEqual(to o: ConfigModel) -> Bool {
        bridgePort == o.bridgePort
        && almaApi == o.almaApi
        && almaModel == o.almaModel
        && almaTimeout == o.almaTimeout
        && almaMaxRetries == o.almaMaxRetries
        && almaRetryDelayMs == o.almaRetryDelayMs
        && onebotApiTimeout == o.onebotApiTimeout
        && accessToken == o.accessToken
        && groupHistorySize == o.groupHistorySize
        && thinkingMessage == o.thinkingMessage
        && showThinking == o.showThinking
        && peopleDir == o.peopleDir
        && dbPath == o.dbPath
    }

    /// Whether saving this change requires a full bridge restart.
    ///
    /// `bridgePort` rebinds the listening socket and `dbPath` reopens the
    /// database, so neither can be hot-reloaded. `almaApi` and `accessToken`
    /// are handled by the bridge's SIGHUP handler (`main.rs`), so they only
    /// need a hot-reload signal rather than a full restart.
    func requiresBridgeRestart(to o: ConfigModel) -> Bool {
        bridgePort != o.bridgePort || dbPath != o.dbPath
    }
}
