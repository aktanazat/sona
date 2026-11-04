import AVFoundation

@_cdecl("sona_microphone_authorized")
public func sonaMicrophoneAuthorized() -> Bool {
    AVCaptureDevice.authorizationStatus(for: .audio) == .authorized
}
