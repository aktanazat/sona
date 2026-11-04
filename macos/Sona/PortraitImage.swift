import AppKit
import ImageIO
import UniformTypeIdentifiers

struct PortraitCrop: Equatable {
    var horizontal: Double = 0.5
    var vertical: Double = 0.5
    var zoom: Double = 1
}

enum PortraitImage {
    static let maximumSourceBytes = 10 * 1024 * 1024
    static let maximumSavedBytes = 1024 * 1024

    static func read(_ url: URL) throws -> Data {
        let granted = url.startAccessingSecurityScopedResource()
        defer { if granted { url.stopAccessingSecurityScopedResource() } }
        let values = try url.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
        guard values.isRegularFile == true, let size = values.fileSize,
              size > 0, size <= maximumSourceBytes else {
            throw PersonFailure(message: "Choose a PNG or JPEG smaller than 10 MB.")
        }
        let data = try Data(contentsOf: url, options: .mappedIfSafe)
        guard data.count <= maximumSourceBytes else {
            throw PersonFailure(message: "Choose a PNG or JPEG smaller than 10 MB.")
        }
        return data
    }

    static func decode(_ data: Data, maximumEdge: Int = 2048) throws -> CGImage {
        guard !data.isEmpty, data.count <= maximumSourceBytes,
              let source = CGImageSourceCreateWithData(data as CFData, [kCGImageSourceShouldCache: false] as CFDictionary),
              let type = CGImageSourceGetType(source),
              [UTType.png.identifier, UTType.jpeg.identifier].contains(type as String),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? NSNumber,
              let height = properties[kCGImagePropertyPixelHeight] as? NSNumber,
              width.int64Value > 0, height.int64Value > 0,
              width.int64Value <= 32768, height.int64Value <= 32768,
              width.int64Value * height.int64Value <= 100_000_000,
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                  kCGImageSourceCreateThumbnailFromImageAlways: true,
                  kCGImageSourceCreateThumbnailWithTransform: true,
                  kCGImageSourceShouldCacheImmediately: true,
                  kCGImageSourceThumbnailMaxPixelSize: maximumEdge,
              ] as CFDictionary) else {
            throw PersonFailure(message: "Couldn't read that photo. Choose a PNG or JPEG.")
        }
        return image
    }

    static func cropped(_ image: CGImage, crop: PortraitCrop) throws -> CGImage {
        let edge = floor(Double(min(image.width, image.height)) / min(3, max(1, crop.zoom)))
        let rectangle = CGRect(
            x: floor((Double(image.width) - edge) * min(1, max(0, crop.horizontal))),
            y: floor((Double(image.height) - edge) * min(1, max(0, crop.vertical))),
            width: edge, height: edge)
        guard let cropped = image.cropping(to: rectangle) else {
            throw PersonFailure(message: "Couldn't crop that photo.")
        }
        let size = min(512, cropped.width, cropped.height)
        guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
              let context = CGContext(
                  data: nil, width: size, height: size, bitsPerComponent: 8,
                  bytesPerRow: size * 4, space: colorSpace,
                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
            throw PersonFailure(message: "Couldn't prepare that photo.")
        }
        context.interpolationQuality = .high
        context.draw(cropped, in: CGRect(x: 0, y: 0, width: size, height: size))
        guard let normalized = context.makeImage() else {
            throw PersonFailure(message: "Couldn't prepare that photo.")
        }
        return normalized
    }

    static func png(_ image: CGImage) throws -> Data {
        let bytes = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(bytes, UTType.png.identifier as CFString, 1, nil) else {
            throw PersonFailure(message: "Couldn't prepare that photo.")
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination), bytes.length <= maximumSavedBytes else {
            throw PersonFailure(message: "That crop is too large. Choose a smaller photo.")
        }
        return bytes as Data
    }
}
