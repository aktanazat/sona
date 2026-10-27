import Foundation
import FoundationModels

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
                let schema = try JSONDecoder().decode(GenerationSchema.self, from: Data(schemaJSON.utf8))
                let response = try await session.respond(
                    to: Prompt { content },
                    schema: schema,
                    options: options
                )
                output = response.content.jsonString
            case Int32(APPLE_LLM_RESPONSE_JSON):
                output = try await session.respond(to: content, options: options).content
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