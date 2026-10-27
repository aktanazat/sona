import Foundation

// Stub implementation when FoundationModels is not available
// This file is compiled via Cargo build script when the build environment
// does not support Apple Intelligence (e.g. older Xcode/SDK).

private typealias ResponsePointer = UnsafeMutablePointer<AppleLLMResponse>

// No FoundationModels in this build, so there is no OS availability to read and
// no reason code to map. Reports the unknown reason; the inference call below
// carries the build-time explanation the user can act on.
@_cdecl("apple_intelligence_status")
public func appleIntelligenceStatus() -> Int32 {
    return 5
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
    let response = ResponsePointer.allocate(capacity: 1)
    response.initialize(to: AppleLLMResponse(
        response: nil, success: 0,
        error_message: strdup("Apple Intelligence is not available in this build (SDK requirement not met).")
    ))
    completion(requestID, response)
}

@_cdecl("cancel_apple_intelligence_generation")
public func cancelAppleIntelligenceGeneration(_ requestID: UInt64) {}

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
