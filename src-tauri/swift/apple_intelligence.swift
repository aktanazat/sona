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
    failureKind: Int32 = APPLE_LLM_FAILURE_OTHER,
    completion: AppleLLMCompletion
) {
    let pointer = ResponsePointer.allocate(capacity: 1)
    pointer.initialize(to: AppleLLMResponse(
        response: output.flatMap(duplicateCString),
        success: output == nil ? 0 : 1,
        error_message: error.flatMap(duplicateCString),
        failure_kind: failureKind
    ))
    completion(id, pointer)
}

/// The APPLE_LLM_FAILURE_* code for an error the model raised.
private func failureKind(_ error: Error) -> Int32 {
    if let stopped = error as? MeetingPassStopped {
        return failureKind(stopped.stop)
    }
    if #available(macOS 27.0, *) {
        guard let error = error as? LanguageModelError else { return APPLE_LLM_FAILURE_OTHER }
        switch error {
        case .guardrailViolation: return APPLE_LLM_FAILURE_GUARDRAIL
        case .contextSizeExceeded: return APPLE_LLM_FAILURE_CONTEXT_WINDOW
        case .refusal: return APPLE_LLM_FAILURE_REFUSAL
        case .unsupportedLanguageOrLocale: return APPLE_LLM_FAILURE_UNSUPPORTED_LANGUAGE
        default: return APPLE_LLM_FAILURE_OTHER
        }
    } else if #available(macOS 26.0, *) {
        return macOS26FailureKind(error)
    }
    return APPLE_LLM_FAILURE_OTHER
}

/// macOS 26 throws `GenerationError`, which macOS 27 replaced with
/// `LanguageModelError`.
@available(macOS, introduced: 26.0, deprecated: 27.0)
private func macOS26FailureKind(_ error: Error) -> Int32 {
    guard let error = error as? LanguageModelSession.GenerationError else { return APPLE_LLM_FAILURE_OTHER }
    switch error {
    case .guardrailViolation: return APPLE_LLM_FAILURE_GUARDRAIL
    case .exceededContextWindowSize: return APPLE_LLM_FAILURE_CONTEXT_WINDOW
    case .refusal: return APPLE_LLM_FAILURE_REFUSAL
    case .unsupportedLanguageOrLocale: return APPLE_LLM_FAILURE_UNSUPPORTED_LANGUAGE
    default: return APPLE_LLM_FAILURE_OTHER
    }
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

/// A meeting pass the safety filter stopped, with what each try did: its
/// stage and the size of a reply that did not fit, never the words.
private struct MeetingPassStopped: LocalizedError {
    let stop: Error
    let tries: [String]

    var errorDescription: String? {
        "\(stop.localizedDescription) Tries: \(tries.joined(separator: "; "))."
    }
}

/// One meeting pass under the guardrails Apple provides for transforming the
/// user's own text.
///
/// Those guardrails stop a violation only for a String reply: guided
/// generation still applies the default ones. Whether they stop a reply
/// varies with the sampled words, so a stopped pass is sampled again, and
/// then asked for text with the schema in its instructions. A text reply
/// that misses the schema reports the guardrail stop, which is what kept
/// this pass from its notes.
@available(macOS 26.0, *)
private func generateMeetingJSON(
    instructions: String,
    content: String,
    schemaJSON: String,
    options: GenerationOptions
) async throws -> String {
    let model = SystemLanguageModel(guardrails: .permissiveContentTransformations)
    let sampled = GenerationOptions(maximumResponseTokens: options.maximumResponseTokens)
    var guardrailStop: Error
    var tries: [String] = []
    do {
        return try await generateJSON(
            session: LanguageModelSession(model: model, instructions: instructions),
            content: content,
            schemaJSON: schemaJSON,
            options: options
        )
    } catch where failureKind(error) == APPLE_LLM_FAILURE_GUARDRAIL {
        guardrailStop = error
        tries.append("guided: safety filter")
    }
    for _ in 0..<2 {
        try Task.checkCancellation()
        do {
            return try await generateJSON(
                session: LanguageModelSession(model: model, instructions: instructions),
                content: content,
                schemaJSON: schemaJSON,
                options: sampled
            )
        } catch where failureKind(error) == APPLE_LLM_FAILURE_GUARDRAIL {
            guardrailStop = error
            tries.append("guided sampled: safety filter")
        }
    }
    let textInstructions = """
        \(instructions)

        Reply with one JSON object that matches this JSON schema. Write nothing before or after it, and no code fence.
        \(schemaJSON)
        """
    let schema = try JSONDecoder().decode(ReplySchema.self, from: Data(schemaJSON.utf8))
    for (stage, textOptions) in [("text", options), ("text sampled", sampled)] {
        try Task.checkCancellation()
        let session = LanguageModelSession(model: model, instructions: textInstructions)
        let reply = try await session.respond(to: Prompt { content }, options: textOptions).content
        let size = "\(reply.utf8.count)-byte reply"
        guard let start = reply.firstIndex(of: "{"), let end = reply.lastIndex(of: "}"), start < end else {
            tries.append("\(stage): \(size) with no JSON object")
            continue
        }
        let json = String(reply[start...end])
        guard let value = try? JSONDecoder().decode(JSONValue.self, from: Data(json.utf8)) else {
            tries.append("\(stage): \(size) that is not valid JSON")
            continue
        }
        guard let mismatch = schema.mismatch(value, definitions: schema.definitions ?? [:]) else {
            return json
        }
        tries.append("\(stage): \(size) that does not fit the schema at \(mismatch)")
    }
    throw MeetingPassStopped(stop: guardrailStop, tries: tries)
}

/// Plain notes, for a meeting whose structured passes the safety filter
/// stopped: a String reply is the one the permissive guardrails govern.
@available(macOS 26.0, *)
private func generateMeetingText(
    instructions: String,
    content: String,
    options: GenerationOptions
) async throws -> String {
    let model = SystemLanguageModel(guardrails: .permissiveContentTransformations)
    let session = LanguageModelSession(model: model, instructions: instructions)
    return try await session.respond(to: Prompt { content }, options: options).content
}

private enum JSONValue: Decodable {
    case object([String: JSONValue])
    case array([JSONValue])
    case string(String)
    case number(Double)
    case boolean(Bool)
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .boolean(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else {
            self = .object(try container.decode([String: JSONValue].self))
        }
    }
}

/// The subset of JSON Schema the meeting reply types emit. Their objects
/// take no other keys; an optional key may hold null.
private final class ReplySchema: Decodable {
    let reference: String?
    let choices: [ReplySchema]?
    let options: [String]?
    let type: String?
    let properties: [String: ReplySchema]?
    let required: [String]?
    let items: ReplySchema?
    let minItems: Int?
    let maxItems: Int?
    let definitions: [String: ReplySchema]?

    private enum CodingKeys: String, CodingKey {
        case reference = "$ref"
        case choices = "anyOf"
        case options = "enum"
        case type, properties, required, items, minItems, maxItems
        case definitions = "$defs"
    }

    /// Where `value` first leaves the schema, as a path of schema keys and
    /// item positions, or nil when it fits.
    func mismatch(_ value: JSONValue, definitions: [String: ReplySchema], at path: String = "$") -> String? {
        if let reference, let name = reference.split(separator: "/").last {
            guard let definition = definitions[String(name)] else { return "\(path) (undefined reference)" }
            return definition.mismatch(value, definitions: definitions, at: path)
        }
        if let choices {
            var misses: [String] = []
            for choice in choices {
                guard let miss = choice.mismatch(value, definitions: definitions, at: path) else { return nil }
                misses.append(miss)
            }
            return misses.max { $0.count < $1.count } ?? "\(path) (no choice)"
        }
        switch (value, options, type) {
        case let (.string(text), options?, _):
            return options.contains(text) ? nil : "\(path) (not one of the allowed values)"
        case (_, _?, _):
            return "\(path) (not one of the allowed values)"
        case let (.object(object), nil, "object"):
            let required = required ?? []
            if let missing = required.first(where: { object[$0] == nil }) {
                return "\(path).\(missing) (missing)"
            }
            for (key, field) in object.sorted(by: { $0.key < $1.key }) {
                guard let property = properties?[key] else { return "\(path) (a key the schema does not have)" }
                if case .null = field, !required.contains(key) { continue }
                if let miss = property.mismatch(field, definitions: definitions, at: "\(path).\(key)") {
                    return miss
                }
            }
            return nil
        case let (.array(array), nil, "array"):
            if array.count < minItems ?? 0 { return "\(path) (too few items)" }
            if array.count > maxItems ?? Int.max { return "\(path) (too many items)" }
            for (index, item) in array.enumerated() {
                if let items, let miss = items.mismatch(item, definitions: definitions, at: "\(path)[\(index)]") {
                    return miss
                }
            }
            return nil
        case (.string, nil, "string"), (.boolean, nil, "boolean"), (.number, nil, "integer"), (.number, nil, "number"):
            return nil
        default:
            return "\(path) (wrong type)"
        }
    }
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
        var kind = APPLE_LLM_FAILURE_OTHER
        do {
            try Task.checkCancellation()
            var options = GenerationOptions(maximumResponseTokens: maxTokens > 0 ? Int(maxTokens) : nil)
            switch responseShape {
            case Int32(APPLE_LLM_RESPONSE_MEETING_NOTES), Int32(APPLE_LLM_RESPONSE_MEETING_LEDGER):
                options.sampling = .greedy
                guard let schemaJSON else {
                    throw NSError(domain: "SonaAppleGeneration", code: 1, userInfo: [
                        NSLocalizedDescriptionKey: "The meeting response format is missing."
                    ])
                }
                output = try await generateMeetingJSON(
                    instructions: instructions,
                    content: content,
                    schemaJSON: schemaJSON,
                    options: options
                )
            case Int32(APPLE_LLM_RESPONSE_MEETING_PLAIN_NOTES):
                output = try await generateMeetingText(
                    instructions: instructions,
                    content: content,
                    options: options
                )
            case Int32(APPLE_LLM_RESPONSE_JSON):
                let session = LanguageModelSession(model: SystemLanguageModel.default, instructions: instructions)
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
                let session = LanguageModelSession(model: SystemLanguageModel.default, instructions: instructions)
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
            kind = failureKind(error)
        }
        guard GenerationTasks.shared.finish(id: requestID) else { return }
        completeGeneration(id: requestID, output: output, error: failure, failureKind: kind, completion: completion)
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