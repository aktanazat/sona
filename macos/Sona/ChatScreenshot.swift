import AppKit
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

/// The preview and the bytes sent by Send describe the same single image.
struct ChatScreenshotDraft: Identifiable {
    let id = UUID()
    let name: String
    let png: Data
    let image: NSImage

    init(image: CGImage, name: String) throws {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(
            data, UTType.png.identifier as CFString, 1, nil)
        else { throw ChatScreenshotError.unreadable }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { throw ChatScreenshotError.unreadable }
        guard data.length <= 2 * 1024 * 1024 else { throw ChatScreenshotError.tooLarge }
        self.name = name
        png = data as Data
        self.image = NSImage(cgImage: image, size: NSSize(width: image.width, height: image.height))
    }

    static func read(_ url: URL) throws -> Self {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
              let image = CGImageSourceCreateThumbnailAtIndex(
                source, 0, [
                    kCGImageSourceCreateThumbnailFromImageAlways: true,
                    kCGImageSourceCreateThumbnailWithTransform: true,
                    kCGImageSourceThumbnailMaxPixelSize: 2048,
                ] as CFDictionary)
        else { throw ChatScreenshotError.unreadable }
        return try Self(image: image, name: url.lastPathComponent)
    }
}

private enum ChatScreenshotError: LocalizedError {
    case unreadable
    case tooLarge

    var errorDescription: String? {
        switch self {
        case .unreadable: return "Couldn't read that screenshot. Choose a PNG or JPEG image."
        case .tooLarge: return "That screenshot is larger than 2 MiB. Crop it or choose a smaller image."
        }
    }
}

/// macOS owns window selection. Nothing is captured before an explicit choice.
@MainActor
final class ChatScreenshotPicker: NSObject, SCContentSharingPickerObserver {
    private var selection: CheckedContinuation<SCContentFilter?, Error>?
    private var panel: NSOpenPanel?

    func window() async throws -> ChatScreenshotDraft? {
        let picker = SCContentSharingPicker.shared
        defer {
            picker.remove(self)
            picker.isActive = false
        }
        let filter: SCContentFilter? = try await withCheckedThrowingContinuation { continuation in
            selection = continuation
            var configuration = SCContentSharingPickerConfiguration()
            configuration.allowedPickerModes = [.singleWindow]
            configuration.allowsChangingSelectedContent = false
            picker.defaultConfiguration = configuration
            picker.add(self)
            picker.isActive = true
            picker.present(using: .window)
        }
        guard let filter else { return nil }
        try Task.checkCancellation()
        let bounds = filter.contentRect
        guard bounds.width > 0, bounds.height > 0 else { throw ChatScreenshotError.unreadable }
        let scale = min(CGFloat(filter.pointPixelScale), 2048 / max(bounds.width, bounds.height))
        let configuration = SCStreamConfiguration()
        configuration.width = max(1, Int((bounds.width * scale).rounded(.down)))
        configuration.height = max(1, Int((bounds.height * scale).rounded(.down)))
        configuration.showsCursor = false
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration)
        try Task.checkCancellation()
        return try ChatScreenshotDraft(image: image, name: "Window screenshot")
    }

    func file() async throws -> ChatScreenshotDraft? {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.png, .jpeg]
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.prompt = "Preview screenshot"
        self.panel = panel
        defer { self.panel = nil }
        let response = await withCheckedContinuation { continuation in
            panel.begin { continuation.resume(returning: $0) }
        }
        guard response == .OK, let url = panel.url else { return nil }
        try Task.checkCancellation()
        return try ChatScreenshotDraft.read(url)
    }

    func cancel() {
        panel?.cancel(nil)
        finish(.success(nil))
        SCContentSharingPicker.shared.isActive = false
    }

    private func finish(_ result: Result<SCContentFilter?, Error>) {
        let pending = selection
        selection = nil
        pending?.resume(with: result)
    }

    nonisolated func contentSharingPicker(
        _ picker: SCContentSharingPicker, didCancelFor stream: SCStream?
    ) {
        Task { @MainActor in finish(.success(nil)) }
    }

    nonisolated func contentSharingPicker(
        _ picker: SCContentSharingPicker, didUpdateWith filter: SCContentFilter, for stream: SCStream?
    ) {
        Task { @MainActor in finish(.success(filter)) }
    }

    nonisolated func contentSharingPickerStartDidFailWithError(_ error: Error) {
        Task { @MainActor in finish(.failure(error)) }
    }
}
