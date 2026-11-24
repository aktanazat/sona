import AVFoundation
import Foundation

/// The microphone Sona records and dictates with, when it is not the system's choice.
struct MicrophoneChoice: Codable, Equatable, Hashable {
    var uid: String
    var name: String
    var bluetooth: Bool

    private static let key = "sona.microphone.preferred"

    init(_ port: AVAudioSessionPortDescription) {
        uid = port.uid
        name = port.portName
        bluetooth = port.portType == .bluetoothHFP
    }

    /// The chosen microphone, or nil for the system's choice.
    static var preferred: MicrophoneChoice? {
        get {
            UserDefaults.standard.data(forKey: key).flatMap { try? JSONDecoder().decode(MicrophoneChoice.self, from: $0) }
        }
        set {
            if let newValue, let data = try? JSONEncoder().encode(newValue) {
                UserDefaults.standard.set(data, forKey: key)
            } else {
                UserDefaults.standard.removeObject(forKey: key)
            }
        }
    }

    /// What a recording session has to allow for the chosen microphone to be offered.
    static var categoryOptions: AVAudioSession.CategoryOptions {
        preferred?.bluetooth == true ? [.allowBluetoothHFP] : []
    }

    /// Routes `session` to the chosen microphone. One that is not connected leaves the
    /// system's choice in place, so recording never fails for a missing headset.
    static func apply(to session: AVAudioSession) throws {
        let port = preferred.flatMap { choice in session.availableInputs?.first { $0.uid == choice.uid } }
        try session.setPreferredInput(port)
    }

    /// The microphones connected now. Reading them needs a category that records; one is
    /// set only while the session is idle.
    static func available() -> [MicrophoneChoice] {
        let session = AVAudioSession.sharedInstance()
        if session.category != .playAndRecord, session.category != .record {
            try? session.setCategory(.playAndRecord, mode: .default, options: [.mixWithOthers, .allowBluetoothHFP])
        }
        return (session.availableInputs ?? []).map(MicrophoneChoice.init)
    }
}
