import AVFoundation
import CryptoKit
import Foundation

/// Where a finished capture lives before it is queued.
struct CapturedAudio: Sendable {
    var url: URL
    var byteLength: Int
    var sha256: String
    var durationMs: Int64
    var codec = RecordingAudioFormat.codec
}

/// What a capture is re-encoded to before it is queued: constant 24 kbps AAC-LC at the
/// capture's rate, about a tenth of its size. Apple's Opus encoder came out larger at
/// the same requested rate, and the Mac's existing decoder already reads this.
///
/// The stream is bare ADTS frames: `AVAudioFile` pads an MPEG-4 file with about 23 KB
/// of free space, which would cost a short voice thought more than its audio does.
enum UploadAudioFormat {
    static let codec = "aac_adts"
    static let fileExtension = "aac"
    static let fileName = "audio.\(fileExtension)"
    static let bitRate = 24_000
}

enum CaptureError: Error {
    case unsupportedFormat
    case conversionFailed
    case writeFailed
}

/// Writes 16 kHz mono signed 16-bit little-endian samples, resampling whatever the
/// hardware or a watch file hands over.
///
/// The file is headerless samples, so a capture cut short by a crash is still every
/// byte written before it; `encodeForUpload` makes the copy that leaves the device.
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

/// Encode a capture-format file as `UploadAudioFormat` at `destination`.
///
/// The capture stays where it is: it is the copy a crash recovers from until the
/// encoded file has a home.
func encodeForUpload(_ audio: CapturedAudio, destination: URL) throws -> CapturedAudio {
    guard let format = PCMResampler.targetFormat,
          let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 16_384),
          let samples = buffer.int16ChannelData
    else { throw CaptureError.unsupportedFormat }
    let frameBytes = RecordingAudioFormat.bytesPerFrame * RecordingAudioFormat.channels
    let source = try FileHandle(forReadingFrom: audio.url)
    defer { try? source.close() }
    if FileManager.default.fileExists(atPath: destination.path) {
        try FileManager.default.removeItem(at: destination)
    }
    let output = try AVAudioFile(
        forWriting: destination,
        settings: [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: Double(RecordingAudioFormat.sampleRateHz),
            AVNumberOfChannelsKey: RecordingAudioFormat.channels,
            AVEncoderBitRateKey: UploadAudioFormat.bitRate,
            AVEncoderBitRateStrategyKey: AVAudioBitRateStrategy_Constant,
        ],
        commonFormat: .pcmFormatInt16, interleaved: true
    )
    do {
        while let bytes = try source.read(upToCount: Int(buffer.frameCapacity) * frameBytes),
              bytes.count >= frameBytes {
            let frames = bytes.count / frameBytes
            bytes.withUnsafeBytes { raw in
                UnsafeMutableRawPointer(samples[0]).copyMemory(
                    from: raw.baseAddress!, byteCount: frames * frameBytes
                )
            }
            buffer.frameLength = AVAudioFrameCount(frames)
            try output.write(from: buffer)
        }
        output.close()
        return try prepareADTSUpload(at: destination, durationMs: audio.durationMs)
    } catch {
        output.close()
        try? FileManager.default.removeItem(at: destination)
        throw error
    }
}

/// Keep a watch's already encoded AAC stream without another lossy encode.
func prepareADTSUpload(at url: URL, durationMs: Int64) throws -> CapturedAudio {
    let file = try AVAudioFile(forReading: url)
    guard file.fileFormat.streamDescription.pointee.mFormatID == kAudioFormatMPEG4AAC,
          file.fileFormat.sampleRate == Double(RecordingAudioFormat.sampleRateHz),
          file.fileFormat.channelCount == AVAudioChannelCount(RecordingAudioFormat.channels)
    else { throw CaptureError.unsupportedFormat }
    file.close()
    try normalizeADTS(at: url)
    let encoded = try FileHandle(forReadingFrom: url)
    defer { try? encoded.close() }
    var hasher = SHA256()
    var byteLength = 0
    while let bytes = try encoded.read(upToCount: 1 << 20), !bytes.isEmpty {
        hasher.update(data: bytes)
        byteLength += bytes.count
    }
    return CapturedAudio(
        url: url, byteLength: byteLength,
        sha256: Base64URL.encode(Data(hasher.finalize())),
        durationMs: durationMs, codec: UploadAudioFormat.codec
    )
}

/// A watch from before compact capture sends WAV; a newer one sends AAC. Decode for
/// the phone's local library either way, but only encode an older watch's upload.
func prepareWatchAudio(
    source: URL, destination: URL, durationMs: Int64?
) throws -> (capture: CapturedAudio, upload: CapturedAudio?) {
    let capture = try transcodeToCaptureFormat(source: source, destination: destination)
    let upload = source.pathExtension == UploadAudioFormat.fileExtension
        ? try prepareADTSUpload(at: source, durationMs: durationMs ?? capture.durationMs) : nil
    return (capture, upload)
}

/// Apple's ADTS writer labels AAC-LC as MPEG-2; its syntax is also MPEG-4 AAC-LC,
/// but Symphonia only recognizes the MPEG-4 sync word. Change that ID bit, not the
/// audio. Work a block at a time so a long recording never has to fit in memory.
private func normalizeADTS(at url: URL) throws {
    let file = try FileHandle(forUpdating: url)
    defer { try? file.close() }
    let length = try file.seekToEnd()
    var offset: UInt64 = 0
    while offset < length {
        try file.seek(toOffset: offset)
        guard var bytes = try file.read(upToCount: 64 * 1024), bytes.count >= 7 else {
            throw CaptureError.conversionFailed
        }
        let advanced = try bytes.withUnsafeMutableBytes { (raw: UnsafeMutableRawBufferPointer) -> Int in
            var cursor = 0
            while cursor + 7 <= raw.count {
                guard raw[cursor] == 0xff, raw[cursor + 1] & 0xf7 == 0xf1,
                      raw[cursor + 2] >> 6 == 1 else { throw CaptureError.unsupportedFormat }
                let size = (Int(raw[cursor + 3] & 3) << 11)
                    | (Int(raw[cursor + 4]) << 3) | Int(raw[cursor + 5] >> 5)
                guard size >= 7, offset + UInt64(cursor + size) <= length else {
                    throw CaptureError.conversionFailed
                }
                raw[cursor + 1] &= 0xf7
                cursor += size
            }
            return cursor
        }
        try file.seek(toOffset: offset)
        try file.write(contentsOf: bytes)
        offset += UInt64(advanced)
    }
}
