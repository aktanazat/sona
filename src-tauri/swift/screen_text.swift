import CoreGraphics
import Foundation
import ImageIO
import Vision

// The words in a picture of a meeting's screen, read on this Mac with Vision.
//
// The core hands over a picture's PNG bytes and gets back each line Vision
// found, with its height as a share of the picture's, so the core can tell a
// slide's title from its small print. Nothing is asked of the person and
// nothing leaves the process. Everything here is private except the C entry
// points, because each Swift bridge is compiled on its own and linked into
// the same binary.

private enum ScreenTextResult: Int32 {
    case ok = 0
    case invalidArgument = 1
    case unreadable = 2
    case recognitionFailed = 3
}

private struct ScreenTextLine: Encodable {
    let text: String
    let height: Double
}

/// The lines in the picture, in the order Vision reads them, as a JSON array
/// of `{"text", "height"}`. An empty array is a picture with no words in it.
@_cdecl("sona_screen_text_read")
public func sonaScreenTextRead(
    _ bytes: UnsafePointer<UInt8>?,
    _ length: UInt,
    _ outJSON: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    guard let outJSON else { return ScreenTextResult.invalidArgument.rawValue }
    outJSON.pointee = nil
    guard let bytes, length > 0,
          let source = CGImageSourceCreateWithData(Data(bytes: bytes, count: Int(length)) as CFData, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
    else {
        return ScreenTextResult.unreadable.rawValue
    }
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = true
    request.automaticallyDetectsLanguage = true
    do {
        try VNImageRequestHandler(cgImage: image, options: [:]).perform([request])
    } catch {
        return ScreenTextResult.recognitionFailed.rawValue
    }
    let lines = (request.results ?? []).compactMap { observation -> ScreenTextLine? in
        guard let candidate = observation.topCandidates(1).first else { return nil }
        return ScreenTextLine(text: candidate.string, height: Double(observation.boundingBox.height))
    }
    guard let json = try? JSONEncoder().encode(lines),
          let text = String(data: json, encoding: .utf8),
          let copy = strdup(text)
    else {
        return ScreenTextResult.recognitionFailed.rawValue
    }
    outJSON.pointee = copy
    return ScreenTextResult.ok.rawValue
}

@_cdecl("sona_screen_text_free")
public func sonaScreenTextFree(_ pointer: UnsafeMutableRawPointer?) {
    free(pointer)
}
