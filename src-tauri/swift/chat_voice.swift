import AVFoundation
import AudioToolbox
import CoreAudio
import Foundation

public typealias VoiceAudioCallback = @convention(c) (
    UnsafeMutableRawPointer, UnsafePointer<Float>, UInt, UInt
) -> Void
public typealias VoiceStatusCallback = @convention(c) (
    UnsafeMutableRawPointer, Int32, UInt64
) -> Void
public typealias VoiceReleaseCallback = @convention(c) (UnsafeMutableRawPointer) -> Void

/// The tap retains this owner until its last callback returns, even across stop.
private final class VoiceCallbacks {
    let context: UnsafeMutableRawPointer
    let audio: VoiceAudioCallback
    let status: VoiceStatusCallback
    let release: VoiceReleaseCallback

    init(_ context: UnsafeMutableRawPointer, _ audio: @escaping VoiceAudioCallback,
         _ status: @escaping VoiceStatusCallback, _ release: @escaping VoiceReleaseCallback) {
        self.context = context
        self.audio = audio
        self.status = status
        self.release = release
    }

    deinit { release(context) }
}

private enum VoiceFailure: Error, LocalizedError {
    case microphonePermission, microphoneUnavailable, formatUnavailable
    case deviceSelection(OSStatus)

    var errorDescription: String? {
        switch self {
        case .microphonePermission:
            "Allow Sona microphone access in System Settings before starting voice chat."
        case .microphoneUnavailable:
            "The selected microphone is unavailable. Choose a microphone in Settings."
        case .formatUnavailable:
            "The microphone cannot provide audio for voice chat."
        case .deviceSelection(let status):
            "The selected microphone could not be opened (\(status))."
        }
    }
}

private func voiceOnMain<T>(_ work: () throws -> T) rethrows -> T {
    if Thread.isMainThread { return try work() }
    return try DispatchQueue.main.sync(execute: work)
}

private func voiceInputDevice(named name: String) throws -> AudioDeviceID {
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size) == noErr else {
        throw VoiceFailure.microphoneUnavailable
    }
    var devices = [AudioDeviceID](repeating: 0, count: Int(size) / MemoryLayout<AudioDeviceID>.size)
    let result = devices.withUnsafeMutableBytes { buffer -> OSStatus in
        guard let base = buffer.baseAddress else { return kAudioHardwareBadObjectError }
        return AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, base)
    }
    guard result == noErr else { throw VoiceFailure.microphoneUnavailable }
    for device in devices {
        var streams = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreams,
            mScope: kAudioDevicePropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain)
        var streamBytes: UInt32 = 0
        guard AudioObjectGetPropertyDataSize(device, &streams, 0, nil, &streamBytes) == noErr,
              streamBytes > 0 else { continue }
        var property = AudioObjectPropertyAddress(
            mSelector: kAudioObjectPropertyName,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var value: CFString = "" as CFString
        var bytes = UInt32(MemoryLayout<CFString>.size)
        if AudioObjectGetPropertyData(device, &property, 0, nil, &bytes, &value) == noErr,
           value as String == name {
            return device
        }
    }
    throw VoiceFailure.microphoneUnavailable
}

/// Capture and speech playback share the voice-processing I/O unit, so the
/// echo canceller receives the exact audio that Sona is playing.
private final class VoiceEngine {
    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private let synthesizer = AVSpeechSynthesizer()
    private let callbacks: VoiceCallbacks
    private var observer: NSObjectProtocol?
    private var tapInstalled = false
    private var playbackGeneration: UInt64 = 0
    private var pendingBuffers = 0
    private var synthesisFinished = false
    private var playbackFormat: AVAudioFormat?
    private var closed = false

    init(callbacks: VoiceCallbacks) { self.callbacks = callbacks }

    func start(microphone: String?) throws -> UInt {
        guard AVCaptureDevice.authorizationStatus(for: .audio) == .authorized else {
            throw VoiceFailure.microphonePermission
        }
        let input = engine.inputNode
        try input.setVoiceProcessingEnabled(true)
        if let microphone {
            guard let unit = input.audioUnit else { throw VoiceFailure.microphoneUnavailable }
            var device = try voiceInputDevice(named: microphone)
            let status = AudioUnitSetProperty(
                unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0,
                &device, UInt32(MemoryLayout<AudioDeviceID>.size))
            guard status == noErr else { throw VoiceFailure.deviceSelection(status) }
        }
        // Enabling voice processing can change this format.
        let format = input.outputFormat(forBus: 0)
        guard format.commonFormat == .pcmFormatFloat32,
              format.channelCount > 0,
              format.sampleRate >= 8_000, format.sampleRate <= 192_000,
              format.sampleRate.rounded() == format.sampleRate else {
            throw VoiceFailure.formatUnavailable
        }
        engine.attach(player)
        engine.connect(player, to: engine.mainMixerNode, format: nil)
        let callbacks = callbacks
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
            guard let samples = buffer.floatChannelData?.pointee,
                  buffer.format == format else {
                callbacks.status(callbacks.context, 1, 0)
                return
            }
            // The first voice-processing output channel is the processed mic.
            callbacks.audio(callbacks.context, samples, UInt(buffer.frameLength), UInt(buffer.stride))
        }
        tapInstalled = true
        engine.prepare()
        try engine.start()
        observer = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main
        ) { [weak self] _ in
            guard let self, !self.closed, !self.engine.isRunning else { return }
            self.callbacks.status(self.callbacks.context, 2, 0)
        }
        return UInt(format.sampleRate)
    }

    func speak(_ text: String, language: String, utterance: UInt64) {
        interrupt()
        let generation = playbackGeneration
        let speech = AVSpeechUtterance(string: text)
        if language != "auto" { speech.voice = AVSpeechSynthesisVoice(language: language) }
        synthesizer.write(speech) { [weak self] buffer in
            DispatchQueue.main.async { [weak self] in
                guard let self, !self.closed, self.playbackGeneration == generation else { return }
                guard let pcm = buffer as? AVAudioPCMBuffer else {
                    self.callbacks.status(self.callbacks.context, 3, 0)
                    return
                }
                if pcm.frameLength == 0 {
                    self.synthesisFinished = true
                    self.finishPlayback(utterance: utterance)
                    return
                }
                if self.playbackFormat != pcm.format {
                    self.engine.connect(self.player, to: self.engine.mainMixerNode, format: pcm.format)
                    self.playbackFormat = pcm.format
                }
                self.pendingBuffers += 1
                self.player.scheduleBuffer(pcm, completionCallbackType: .dataPlayedBack) { [weak self] _ in
                    DispatchQueue.main.async { [weak self] in
                        guard let self, !self.closed, self.playbackGeneration == generation else { return }
                        self.pendingBuffers -= 1
                        self.finishPlayback(utterance: utterance)
                    }
                }
                if !self.player.isPlaying { self.player.play() }
            }
        }
    }

    private func finishPlayback(utterance: UInt64) {
        if synthesisFinished && pendingBuffers == 0 {
            synthesisFinished = false
            callbacks.status(callbacks.context, 4, utterance)
        }
    }

    func interrupt() {
        playbackGeneration &+= 1
        synthesizer.stopSpeaking(at: .immediate)
        player.stop()
        pendingBuffers = 0
        synthesisFinished = false
    }

    func stop() {
        guard !closed else { return }
        closed = true
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = nil
        interrupt()
        if tapInstalled {
            engine.inputNode.removeTap(onBus: 0)
            tapInstalled = false
        }
        engine.stop()
    }
}

@_cdecl("sona_voice_start")
public func sonaVoiceStart(
    _ microphone: UnsafePointer<CChar>?, _ context: UnsafeMutableRawPointer,
    _ audio: @escaping VoiceAudioCallback, _ status: @escaping VoiceStatusCallback,
    _ release: @escaping VoiceReleaseCallback, _ sampleRate: UnsafeMutablePointer<UInt>,
    _ errorText: UnsafeMutablePointer<CChar>, _ errorCapacity: UInt
) -> UnsafeMutableRawPointer? {
    let callbacks = VoiceCallbacks(context, audio, status, release)
    let name = microphone.map { String(cString: $0) }
    return voiceOnMain {
        let voice = VoiceEngine(callbacks: callbacks)
        do {
            sampleRate.pointee = try voice.start(microphone: name)
            return Unmanaged.passRetained(voice).toOpaque()
        } catch {
            voice.stop()
            if errorCapacity > 0 {
                let bytes = Array(error.localizedDescription.utf8.prefix(Int(errorCapacity) - 1))
                for (index, byte) in bytes.enumerated() { errorText[index] = CChar(bitPattern: byte) }
                errorText[bytes.count] = 0
            }
            return nil
        }
    }
}

@_cdecl("sona_voice_speak")
public func sonaVoiceSpeak(
    _ handle: UnsafeMutableRawPointer, _ text: UnsafePointer<CChar>,
    _ language: UnsafePointer<CChar>, _ utterance: UInt64
) {
    let message = String(cString: text)
    let locale = String(cString: language)
    voiceOnMain {
        Unmanaged<VoiceEngine>.fromOpaque(handle).takeUnretainedValue()
            .speak(message, language: locale, utterance: utterance)
    }
}

@_cdecl("sona_voice_interrupt")
public func sonaVoiceInterrupt(_ handle: UnsafeMutableRawPointer) {
    voiceOnMain { Unmanaged<VoiceEngine>.fromOpaque(handle).takeUnretainedValue().interrupt() }
}

@_cdecl("sona_voice_stop")
public func sonaVoiceStop(_ handle: UnsafeMutableRawPointer) {
    voiceOnMain { Unmanaged<VoiceEngine>.fromOpaque(handle).takeRetainedValue().stop() }
}
