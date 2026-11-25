import AVFoundation
import CryptoKit
import Foundation

/// Where a finished capture lives before it is queued.
struct CapturedAudio: Sendable {
    var url: URL
    var byteLength: Int
    var sha256: String
    var durationMs: Int64
}

enum CaptureError: Error {
    case unsupportedFormat
    case conversionFailed
    case writeFailed
}

/// Writes 16 kHz mono signed 16-bit little-endian samples, resampling whatever the
/// hardware or a watch file hands over.
///
/// The file this produces is the exact byte sequence the upload chunks carry, so no
/// container is ever parsed on the way out.
final class PCMResampler {
    static let targetFormat = AVAudioFormat(
        commonFormat: .pcmFormatInt16,
        sampleRate: Double(RecordingAudioFormat.sampleRateHz),
        channels: AVAudioChannelCount(RecordingAudioFormat.channels),
        interleaved: true
    )

    private let url: URL
    private let handle: FileHandle
    private let converter: AVAudioConverter
    private var hasher = SHA256()
    private var byteLength = 0
    private var writeFailed = false

    init(url: URL, inputFormat: AVAudioFormat, existingFile: Bool = false) throws {
        guard let target = PCMResampler.targetFormat,
              let converter = AVAudioConverter(from: inputFormat, to: target)
        else { throw CaptureError.unsupportedFormat }
        if !existingFile && !FileManager.default.createFile(atPath: url.path, contents: nil) {
            throw CaptureError.writeFailed
        }
        guard let handle = try? FileHandle(forWritingTo: url) else {
            throw CaptureError.writeFailed
        }
        self.url = url
        self.handle = handle
        self.converter = converter
    }

    func append(_ input: AVAudioPCMBuffer) throws {
        guard !writeFailed else { throw CaptureError.writeFailed }
        guard let target = PCMResampler.targetFormat else { throw CaptureError.unsupportedFormat }
        let ratio = target.sampleRate / input.format.sampleRate
        let capacity = AVAudioFrameCount(Double(input.frameLength) * ratio) + 1024
        guard let output = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else {
            throw CaptureError.conversionFailed
        }
        var consumed = false
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) { _, inputStatus in
            if consumed {
                inputStatus.pointee = .noDataNow
                return nil
            }
            consumed = true
            inputStatus.pointee = .haveData
            return input
        }
        guard status != .error, conversionError == nil else { throw CaptureError.conversionFailed }
        try write(output)
    }

    /// Flush the converter's tail so the last partial resampling window is not dropped.
    func finish() throws -> CapturedAudio {
        defer { try? handle.close() }
        guard !writeFailed else { throw CaptureError.writeFailed }
        guard let target = PCMResampler.targetFormat,
              let output = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: 4096)
        else { throw CaptureError.unsupportedFormat }
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) { _, inputStatus in
            inputStatus.pointee = .endOfStream
            return nil
        }
        if status != .error, conversionError == nil {
            try write(output)
        }
        let frames = byteLength / (RecordingAudioFormat.bytesPerFrame * RecordingAudioFormat.channels)
        return CapturedAudio(
            url: url,
            byteLength: byteLength,
            sha256: Base64URL.encode(Data(hasher.finalize())),
            durationMs: Int64(frames * 1000 / RecordingAudioFormat.sampleRateHz)
        )
    }

    private func write(_ buffer: AVAudioPCMBuffer) throws {
        guard buffer.frameLength > 0, let samples = buffer.int16ChannelData else { return }
        let count = Int(buffer.frameLength) * RecordingAudioFormat.channels
        let bytes = samples[0].withMemoryRebound(to: UInt8.self, capacity: count * 2) { pointer in
            Data(bytes: pointer, count: count * RecordingAudioFormat.bytesPerFrame)
        }
        do {
            try handle.write(contentsOf: bytes)
        } catch {
            writeFailed = true
            throw CaptureError.writeFailed
        }
        hasher.update(data: bytes)
        byteLength += bytes.count
    }
}

/// Decode any file `AVAudioFile` can open into the capture format.
///
/// The watch records a WAV with `AVAudioRecorder`; the phone owns the one resampling
/// path, so the watch file becomes indistinguishable from a phone recording here.
func transcodeToCaptureFormat(source: URL, destination: URL) throws -> CapturedAudio {
    let file = try AVAudioFile(forReading: source)
    let resampler = try PCMResampler(url: destination, inputFormat: file.processingFormat)
    let frameCapacity: AVAudioFrameCount = 8192
    while file.framePosition < file.length {
        guard let buffer = AVAudioPCMBuffer(
            pcmFormat: file.processingFormat, frameCapacity: frameCapacity
        ) else { throw CaptureError.conversionFailed }
        try file.read(into: buffer, frameCount: frameCapacity)
        if buffer.frameLength == 0 { break }
        try resampler.append(buffer)
    }
    return try resampler.finish()
}
