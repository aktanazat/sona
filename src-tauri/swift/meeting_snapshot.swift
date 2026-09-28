import AppKit
import CoreGraphics
import Dispatch
import Foundation
import ScreenCaptureKit

// One still of the meeting app's window, for a call that shares a screen.
//
// The core asks for a picture of whichever meeting window is on screen now and
// gets back straight-alpha RGBA rows, top row first, `width * 4` bytes each.
// Everything here is private except the three C entry points, because each
// Swift bridge is compiled on its own and linked into the same binary.

private enum SnapshotResult: Int32 {
    case ok = 0
    case invalidArgument = 1
    case unsupported = 2
    case permissionDenied = 3
    case sourceUnavailable = 4
    case streamFailure = 6
}

private let snapshotMaximumBundleIDs = 64
private let snapshotMaximumBundleIDCharacters = 255
private let snapshotMinimumPixelWidth: UInt32 = 32
/// A window smaller than this is a toolbar, a floating self view, or a
/// minimized strip, never the shared screen.
private let snapshotMinimumWindowWidth: CGFloat = 200
private let snapshotMinimumWindowHeight: CGFloat = 150
/// ScreenCaptureKit answers in well under a second; this only keeps a wedged
/// call from holding the core's thread forever.
private let snapshotTimeoutSeconds: Double = 10

/// The apps a call can run in when the core names none on screen: the meeting
/// apps first, then the browsers a web call runs in.
private let snapshotFallbackBundleIDs: [String] = [
    "us.zoom.xos",
    "com.microsoft.teams2",
    "com.microsoft.teams",
    "com.cisco.webexmeetingsapp",
    "com.cisco.webex",
    "com.tinyspeck.slackmacgap",
    "com.apple.facetime",
    "com.google.chrome",
    "com.google.chrome.canary",
    "com.apple.safari",
    "com.microsoft.edgemac",
    "company.thebrowser.browser",
    "org.mozilla.firefox",
]

private final class SnapshotCompletion<Value>: @unchecked Sendable {
    private let semaphore = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var value: Value?
    private var result: Int32 = SnapshotResult.streamFailure.rawValue

    func succeed(_ value: Value) {
        lock.lock()
        self.value = value
        result = SnapshotResult.ok.rawValue
        lock.unlock()
        semaphore.signal()
    }

    func fail(_ result: Int32) {
        lock.lock()
        self.result = result
        lock.unlock()
        semaphore.signal()
    }

    func wait() -> (Value?, Int32) {
        guard semaphore.wait(timeout: .now() + snapshotTimeoutSeconds) == .success else {
            return (nil, SnapshotResult.streamFailure.rawValue)
        }
        lock.lock()
        defer { lock.unlock() }
        return (value, result)
    }
}

/// The candidates in the order the core ranked them, then the fallback apps.
/// Unlike the audio filter, order matters here: the first app with a window
/// on screen is the one photographed.
private func orderedSnapshotBundleIDs(_ rawBundleIDs: UnsafeRawPointer?, count: UInt) -> [String]? {
    guard count <= UInt(snapshotMaximumBundleIDs) else {
        return nil
    }
    guard count == 0 || rawBundleIDs != nil else {
        return nil
    }
    let pointers = rawBundleIDs?.assumingMemoryBound(to: UnsafePointer<CChar>?.self)
    var seen = Set<String>()
    var ordered: [String] = []
    ordered.reserveCapacity(Int(count) + snapshotFallbackBundleIDs.count)
    for index in 0..<Int(count) {
        guard let pointer = pointers?[index],
              let bundleID = String(validatingUTF8: pointer),
              !bundleID.isEmpty,
              bundleID.count <= snapshotMaximumBundleIDCharacters
        else {
            return nil
        }
        let lowered = bundleID.lowercased()
        if seen.insert(lowered).inserted {
            ordered.append(lowered)
        }
    }
    if ordered.isEmpty {
        ordered = snapshotFallbackBundleIDs
    }
    return ordered
}

private func snapshotResult(for error: Error) -> Int32 {
    let nsError = error as NSError
    guard nsError.domain == SCStreamErrorDomain else {
        return SnapshotResult.streamFailure.rawValue
    }
    switch nsError.code {
    case -3801, -3803:
        return SnapshotResult.permissionDenied.rawValue
    case -3806, -3813, -3814, -3815:
        return SnapshotResult.sourceUnavailable.rawValue
    default:
        return SnapshotResult.streamFailure.rawValue
    }
}

/// The largest ordinary window of the first candidate app that has one on
/// screen. A window on another Space, minimized, or hidden is not on screen,
/// so a call the person has put away is never photographed.
@available(macOS 14.0, *)
private func snapshotWindow(in content: SCShareableContent, bundleIDs: [String]) -> (SCWindow, String)? {
    let ownProcessID = ProcessInfo.processInfo.processIdentifier
    var largest: [String: SCWindow] = [:]
    for window in content.windows {
        guard window.windowLayer == 0,
              window.isOnScreen,
              window.frame.width >= snapshotMinimumWindowWidth,
              window.frame.height >= snapshotMinimumWindowHeight,
              let application = window.owningApplication,
              application.processID != ownProcessID
        else {
            continue
        }
        let bundleID = application.bundleIdentifier.lowercased()
        if let current = largest[bundleID],
           current.frame.width * current.frame.height >= window.frame.width * window.frame.height {
            continue
        }
        largest[bundleID] = window
    }
    for bundleID in bundleIDs {
        if let window = largest[bundleID] {
            return (window, bundleID)
        }
    }
    return nil
}

@available(macOS 14.0, *)
private func shareableSnapshotContent() -> (SCShareableContent?, Int32) {
    let completion = SnapshotCompletion<SCShareableContent>()
    SCShareableContent.getExcludingDesktopWindows(true, onScreenWindowsOnly: true) { content, error in
        if let content {
            completion.succeed(content)
        } else {
            completion.fail(error.map(snapshotResult(for:)) ?? SnapshotResult.streamFailure.rawValue)
        }
    }
    return completion.wait()
}

@available(macOS 14.0, *)
private func captureSnapshotImage(of window: SCWindow, maximumPixelWidth: Int) -> (CGImage?, Int32) {
    let filter = SCContentFilter(desktopIndependentWindow: window)
    let scale = max(CGFloat(filter.pointPixelScale), 1)
    var width = window.frame.width * scale
    var height = window.frame.height * scale
    let limit = CGFloat(maximumPixelWidth)
    if max(width, height) > limit {
        let ratio = limit / max(width, height)
        width *= ratio
        height *= ratio
    }
    let configuration = SCStreamConfiguration()
    configuration.width = max(1, Int(width.rounded()))
    configuration.height = max(1, Int(height.rounded()))
    configuration.showsCursor = false
    let completion = SnapshotCompletion<CGImage>()
    SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration) { image, error in
        if let image {
            completion.succeed(image)
        } else {
            completion.fail(error.map(snapshotResult(for:)) ?? SnapshotResult.streamFailure.rawValue)
        }
    }
    return completion.wait()
}

/// The image as straight-alpha sRGB RGBA rows in a `malloc` buffer the caller
/// frees with `sona_meeting_snapshot_free`. A window's rounded corners stay
/// transparent.
private func straightRGBA(_ image: CGImage, maximumPixelWidth: Int) -> (UnsafeMutablePointer<UInt8>, Int, Int)? {
    var width = image.width
    var height = image.height
    guard width > 0, height > 0 else {
        return nil
    }
    if max(width, height) > maximumPixelWidth {
        let ratio = Double(maximumPixelWidth) / Double(max(width, height))
        width = max(1, Int((Double(width) * ratio).rounded()))
        height = max(1, Int((Double(height) * ratio).rounded()))
    }
    let bytesPerRow = width * 4
    let (byteCount, overflow) = bytesPerRow.multipliedReportingOverflow(by: height)
    guard !overflow, let raw = calloc(byteCount, 1) else {
        return nil
    }
    guard let space = CGColorSpace(name: CGColorSpace.sRGB),
          let context = CGContext(
              data: raw,
              width: width,
              height: height,
              bitsPerComponent: 8,
              bytesPerRow: bytesPerRow,
              space: space,
              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue
          )
    else {
        free(raw)
        return nil
    }
    context.interpolationQuality = .high
    context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
    let pixels = raw.assumingMemoryBound(to: UInt8.self)
    for offset in stride(from: 0, to: byteCount, by: 4) {
        let alpha = Int(pixels[offset + 3])
        if alpha == 0 || alpha == 255 {
            continue
        }
        for channel in 0..<3 {
            let value = (Int(pixels[offset + channel]) * 255 + alpha / 2) / alpha
            pixels[offset + channel] = UInt8(min(value, 255))
        }
    }
    return (pixels, width, height)
}

private func copyBundleID(_ bundleID: String, into buffer: UnsafeMutablePointer<CChar>, capacity: UInt) {
    let bytes = Array(bundleID.utf8.prefix(Int(capacity) - 1))
    for (index, byte) in bytes.enumerated() {
        buffer[index] = CChar(bitPattern: byte)
    }
    buffer[bytes.count] = 0
}

@_cdecl("sona_meeting_snapshot_probe")
public func sonaMeetingSnapshotProbe() -> Int32 {
    guard #available(macOS 14.0, *) else {
        return SnapshotResult.unsupported.rawValue
    }
    return CGPreflightScreenCaptureAccess()
        ? SnapshotResult.ok.rawValue
        : SnapshotResult.permissionDenied.rawValue
}

/// Photographs the first candidate app's largest on-screen window, at most
/// `maximumPixelWidth` pixels wide. Never asks for Screen Recording: without
/// the grant it answers `permissionDenied` and shows no prompt.
@_cdecl("sona_meeting_snapshot_capture")
public func sonaMeetingSnapshotCapture(
    _ bundleIDs: UnsafeRawPointer?,
    _ bundleIDCount: UInt,
    _ maximumPixelWidth: UInt32,
    _ outPixels: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outWidth: UnsafeMutablePointer<UInt32>?,
    _ outHeight: UnsafeMutablePointer<UInt32>?,
    _ outBundleID: UnsafeMutablePointer<CChar>?,
    _ outBundleIDCapacity: UInt
) -> Int32 {
    guard let outPixels,
          let outWidth,
          let outHeight,
          let outBundleID,
          outBundleIDCapacity > 0,
          maximumPixelWidth >= snapshotMinimumPixelWidth,
          let ordered = orderedSnapshotBundleIDs(bundleIDs, count: bundleIDCount)
    else {
        return SnapshotResult.invalidArgument.rawValue
    }
    outPixels.pointee = nil
    outWidth.pointee = 0
    outHeight.pointee = 0
    outBundleID[0] = 0
    guard #available(macOS 14.0, *) else {
        return SnapshotResult.unsupported.rawValue
    }
    guard CGPreflightScreenCaptureAccess() else {
        return SnapshotResult.permissionDenied.rawValue
    }
    let (content, listed) = shareableSnapshotContent()
    guard let content else {
        return listed
    }
    guard let found = snapshotWindow(in: content, bundleIDs: ordered) else {
        return SnapshotResult.sourceUnavailable.rawValue
    }
    let (image, captured) = captureSnapshotImage(of: found.0, maximumPixelWidth: Int(maximumPixelWidth))
    guard let image else {
        return captured
    }
    guard let frame = straightRGBA(image, maximumPixelWidth: Int(maximumPixelWidth)) else {
        return SnapshotResult.streamFailure.rawValue
    }
    guard let pixelWidth = UInt32(exactly: frame.1), let pixelHeight = UInt32(exactly: frame.2) else {
        free(frame.0)
        return SnapshotResult.streamFailure.rawValue
    }
    outPixels.pointee = frame.0
    outWidth.pointee = pixelWidth
    outHeight.pointee = pixelHeight
    copyBundleID(found.1, into: outBundleID, capacity: outBundleIDCapacity)
    return SnapshotResult.ok.rawValue
}

@_cdecl("sona_meeting_snapshot_free")
public func sonaMeetingSnapshotFree(_ pixels: UnsafeMutablePointer<UInt8>?) {
    free(pixels)
}
