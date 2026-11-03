import Foundation
import FoundationModels
import AppKit
import CoreGraphics

@available(macOS 26.0, *)
@Generable
private struct CleanedTranscript: Sendable {
    let cleanedText: String
}


private typealias ResponsePointer = UnsafeMutablePointer<AppleLLMResponse>

private func duplicateCString(_ text: String) -> UnsafeMutablePointer<CChar>? {
    return text.withCString { basePointer in
        guard let duplicated = strdup(basePointer) else {
            return nil
        }
        return duplicated
    }
}

/// The refusal a caller can act on, keyed by the status codes in
/// apple_intelligence_bridge.h. Three of these are different instructions, not
/// three ways of saying "unavailable".
private func unavailableMessage(_ status: Int32) -> String {
    switch status {
    case 1:
        return "Apple Intelligence is switched off in System Settings > Apple Intelligence & Siri."
    case 2:
        return "Apple Intelligence is still downloading its model."
    case 3:
        return "This Mac is not eligible for Apple Intelligence."
    case 4:
        return "Apple Intelligence requires macOS 26 or newer."
    default:
        return "Apple Intelligence is unavailable for an unrecognized reason."
    }
}

/// Status codes are documented once, in apple_intelligence_bridge.h.
@_cdecl("apple_intelligence_status")
public func appleIntelligenceStatus() -> Int32 {
    guard #available(macOS 26.0, *) else {
        return 4
    }

    switch SystemLanguageModel.default.availability {
    case .available:
        return 0
    case .unavailable(.appleIntelligenceNotEnabled):
        return 1
    case .unavailable(.modelNotReady):
        return 2
    case .unavailable(.deviceNotEligible):
        return 3
    case .unavailable:
        return 5
    }
}

private final class GenerationTasks: @unchecked Sendable {
    static let shared = GenerationTasks()
    private let lock = NSLock()
    private var tasks: [UInt64: Task<Void, Never>] = [:]

    func start(id: UInt64, operation: @escaping @Sendable () async -> Void) {
        lock.lock()
        tasks[id] = Task.detached(priority: .userInitiated, operation: operation)
        lock.unlock()
    }

    func finish(id: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return tasks.removeValue(forKey: id) != nil
    }

    func cancel(id: UInt64) {
        lock.lock()
        let task = tasks.removeValue(forKey: id)
        lock.unlock()
        task?.cancel()
    }
}

private func completeGeneration(
    id: UInt64,
    output: String?,
    error: String?,
    completion: AppleLLMCompletion
) {
    let pointer = ResponsePointer.allocate(capacity: 1)
    pointer.initialize(to: AppleLLMResponse(
        response: output.flatMap(duplicateCString),
        success: output == nil ? 0 : 1,
        error_message: error.flatMap(duplicateCString)
    ))
    completion(id, pointer)
}

@available(macOS 26.0, *)
private func generateJSON(
    session: LanguageModelSession,
    content: String,
    schemaJSON: String,
    options: GenerationOptions
) async throws -> String {
    let schema = try JSONDecoder().decode(GenerationSchema.self, from: Data(schemaJSON.utf8))
    let response = try await session.respond(
        to: Prompt { content },
        schema: schema,
        options: options
    )
    return response.content.jsonString
}

@_cdecl("start_apple_intelligence_generation")
public func startAppleIntelligenceGeneration(
    _ requestID: UInt64,
    _ systemPrompt: UnsafePointer<CChar>,
    _ userContent: UnsafePointer<CChar>,
    _ maxTokens: Int32,
    _ responseShape: Int32,
    _ jsonSchema: UnsafePointer<CChar>?,
    _ completion: AppleLLMCompletion
) {
    let instructions = String(cString: systemPrompt)
    let content = String(cString: userContent)
    let schemaJSON = jsonSchema.map { String(cString: $0) }
    guard #available(macOS 26.0, *) else {
        completeGeneration(id: requestID, output: nil, error: unavailableMessage(4), completion: completion)
        return
    }
    let status = appleIntelligenceStatus()
    guard status == 0 else {
        completeGeneration(id: requestID, output: nil, error: unavailableMessage(status), completion: completion)
        return
    }
    GenerationTasks.shared.start(id: requestID) {
        var output: String?
        var failure: String?
        do {
            try Task.checkCancellation()
            let session = LanguageModelSession(model: SystemLanguageModel.default, instructions: instructions)
            var options = GenerationOptions(maximumResponseTokens: maxTokens > 0 ? Int(maxTokens) : nil)
            switch responseShape {
            case Int32(APPLE_LLM_RESPONSE_MEETING_NOTES), Int32(APPLE_LLM_RESPONSE_MEETING_LEDGER):
                options.sampling = .greedy
                guard let schemaJSON else {
                    throw NSError(domain: "SonaAppleGeneration", code: 1, userInfo: [
                        NSLocalizedDescriptionKey: "The meeting response format is missing."
                    ])
                }
                output = try await generateJSON(
                    session: session,
                    content: content,
                    schemaJSON: schemaJSON,
                    options: options
                )
            case Int32(APPLE_LLM_RESPONSE_JSON):
                if let schemaJSON {
                    options.sampling = .greedy
                    output = try await generateJSON(
                        session: session,
                        content: content,
                        schemaJSON: schemaJSON,
                        options: options
                    )
                } else {
                    output = try await session.respond(to: content, options: options).content
                }
            case Int32(APPLE_LLM_RESPONSE_TEXT):
                do {
                    let response = try await session.respond(to: content, generating: CleanedTranscript.self, options: options)
                    output = response.content.cleanedText
                } catch {
                    try Task.checkCancellation()
                    output = try await session.respond(to: content, options: options).content
                }
            default:
                failure = "The Apple Intelligence response format is unsupported."
            }
            try Task.checkCancellation()
        } catch {
            failure = error.localizedDescription
        }
        guard GenerationTasks.shared.finish(id: requestID) else { return }
        completeGeneration(id: requestID, output: output, error: failure, completion: completion)
    }
}

@_cdecl("cancel_apple_intelligence_generation")
public func cancelAppleIntelligenceGeneration(_ requestID: UInt64) {
    GenerationTasks.shared.cancel(id: requestID)
}

@_cdecl("free_apple_llm_response")
public func freeAppleLLMResponse(_ response: UnsafeMutablePointer<AppleLLMResponse>?) {
    guard let response = response else { return }

    if let responseStr = response.pointee.response {
        free(UnsafeMutablePointer(mutating: responseStr))
    }

    if let errorStr = response.pointee.error_message {
        free(UnsafeMutablePointer(mutating: errorStr))
    }

    response.deallocate()
}

private final class QuietReviewEnvironment: @unchecked Sendable {
    static let shared = QuietReviewEnvironment()
    private let lock = NSLock()
    private var sleeping = false
    private var sessionActive = false
    private var workspaceObservers: [NSObjectProtocol] = []

    private init() {
        if let session = CGSessionCopyCurrentDictionary() as? [String: Any] {
            sessionActive = session[kCGSessionOnConsoleKey as String] as? Bool == true
                && session[kCGSessionLoginDoneKey as String] as? Bool == true
        }
        let workspace = NSWorkspace.shared.notificationCenter
        workspaceObservers.append(workspace.addObserver(
            forName: NSWorkspace.willSleepNotification, object: nil, queue: nil
        ) { [weak self] _ in
            self?.suspend(sleeping: true)
        })
        workspaceObservers.append(workspace.addObserver(
            forName: NSWorkspace.didWakeNotification, object: nil, queue: nil
        ) { [weak self] _ in
            guard let self else { return }
            let session = CGSessionCopyCurrentDictionary() as? [String: Any]
            self.lock.lock()
            self.sleeping = false
            self.sessionActive = session?[kCGSessionOnConsoleKey as String] as? Bool == true
                && session?[kCGSessionLoginDoneKey as String] as? Bool == true
            self.lock.unlock()
        })
        workspaceObservers.append(workspace.addObserver(
            forName: NSWorkspace.sessionDidResignActiveNotification, object: nil, queue: nil
        ) { [weak self] _ in
            self?.suspend(sleeping: false)
        })
        workspaceObservers.append(workspace.addObserver(
            forName: NSWorkspace.sessionDidBecomeActiveNotification, object: nil, queue: nil
        ) { [weak self] _ in
            guard let self else { return }
            self.lock.lock()
            self.sessionActive = true
            self.lock.unlock()
        })
    }

    private func suspend(sleeping: Bool) {
        lock.lock()
        self.sleeping = self.sleeping || sleeping
        sessionActive = false
        lock.unlock()
        sona_quiet_review_interrupt()
    }

    func status() -> Int32 {
        guard let session = CGSessionCopyCurrentDictionary() as? [String: Any],
              session[kCGSessionOnConsoleKey as String] as? Bool == true,
              session[kCGSessionLoginDoneKey as String] as? Bool == true else { return 1 }
        lock.lock()
        let available = !sleeping && sessionActive
        lock.unlock()
        guard available else { return 1 }
        guard let anyInput = CGEventType(rawValue: UInt32.max) else { return 3 }
        let idle = CGEventSource.secondsSinceLastEventType(.combinedSessionState, eventType: anyInput)
        guard idle.isFinite, idle >= 600 else { return 2 }
        return 0
    }
}

@_cdecl("sona_quiet_review_environment")
public func sonaQuietReviewEnvironment() -> Int32 {
    return QuietReviewEnvironment.shared.status()
}