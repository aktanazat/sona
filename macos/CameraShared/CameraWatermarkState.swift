import Foundation

/// Only a camera choice and short-lived recording state cross the app group.
/// Neither frames, audio, meeting names nor transcripts are stored here.
struct CameraWatermarkState: Codable {
    static let group = "AAVB324H37.com.aktanazat.sona.camera"
    static let extensionID = "com.aktanazat.sona.mac.camera"
    static let cameraDeviceID = "A3EB45B7-8BE0-4A91-A64F-77DC59C1197D"
    static let lifetime: TimeInterval = 6

    var watermarkEnabled = false
    var recording = false
    var cameraID: String?
    var updatedAt: TimeInterval = 0

    func showsNotice(at now: TimeInterval) -> Bool {
        watermarkEnabled && recording && now >= updatedAt && now - updatedAt < Self.lifetime
    }

    static func url(_ name: String) throws -> URL {
        guard let groupURL = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group) else {
            throw CameraWatermarkError.unavailableGroup
        }
        return groupURL.appendingPathComponent(name, isDirectory: false)
    }

    static func read() throws -> Self {
        try JSONDecoder().decode(Self.self, from: Data(contentsOf: url("camera-state.json")))
    }

    func write() throws {
        try JSONEncoder().encode(self).write(to: Self.url("camera-state.json"), options: .atomic)
    }
}

struct CameraWatermarkStatus: Codable {
    let message: String
    let streaming: Bool
    let updatedAt: TimeInterval

    func write() throws {
        try JSONEncoder().encode(self).write(to: CameraWatermarkState.url("camera-status.json"), options: .atomic)
    }

    static func read() throws -> Self {
        try JSONDecoder().decode(Self.self, from: Data(contentsOf: CameraWatermarkState.url("camera-status.json")))
    }
}

enum CameraWatermarkError: LocalizedError {
    case unavailableGroup
    case noCamera
    case permissionRequired
    case captureUnavailable
    case allocationFailed
    case unsupportedFormat

    var errorDescription: String? {
        switch self {
        case .unavailableGroup: "The shared camera settings are unavailable. Reinstall the signed Sona app."
        case .noCamera: "Choose a connected physical camera in Sona's Meeting settings."
        case .permissionRequired: "Allow camera access in System Settings before using Sona Camera."
        case .captureUnavailable: "The selected camera cannot provide video. Check its connection and other apps using it."
        case .allocationFailed: "Sona Camera could not allocate a video frame."
        case .unsupportedFormat: "This camera format is not supported."
        }
    }
}
