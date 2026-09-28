import CoreImage
import CoreMedia
import CoreText
import CoreVideo
import Foundation

/// One pool and one pre-rendered label per stream, not per frame.
final class CameraFrameRenderer {
    static let width = 1280
    static let height = 720
    static let duration = CMTime(value: 1, timescale: 30)
    let formatDescription: CMFormatDescription
    private let pool: CVPixelBufferPool
    private let context = CIContext(options: [.cacheIntermediates: false])
    private let badge: CIImage
    private let bounds = CGRect(x: 0, y: 0, width: CGFloat(CameraFrameRenderer.width), height: CGFloat(CameraFrameRenderer.height))
    private let colorSpace = CGColorSpaceCreateDeviceRGB()
    private let allocation = [kCVPixelBufferPoolAllocationThresholdKey: 3] as CFDictionary
    private let background = CIImage(color: .black).cropped(to: CGRect(x: 0, y: 0, width: CGFloat(CameraFrameRenderer.width), height: CGFloat(CameraFrameRenderer.height)))

    static func makeFormatDescription() throws -> CMFormatDescription {
        var format: CMFormatDescription?
        guard CMVideoFormatDescriptionCreate(allocator: kCFAllocatorDefault,
                                             codecType: kCVPixelFormatType_32BGRA,
                                             width: Int32(Self.width), height: Int32(Self.height),
                                             extensions: nil, formatDescriptionOut: &format) == noErr,
              let format else { throw CameraWatermarkError.unsupportedFormat }
        return format
    }

    init() throws {
        formatDescription = try Self.makeFormatDescription()
        var created: CVPixelBufferPool?
        let attributes: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey: Self.width, kCVPixelBufferHeightKey: Self.height,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        guard CVPixelBufferPoolCreate(kCFAllocatorDefault, nil, attributes as CFDictionary, &created) == kCVReturnSuccess,
              let created else { throw CameraWatermarkError.allocationFailed }
        pool = created
        guard let image = Self.makeBadge() else { throw CameraWatermarkError.allocationFailed }
        badge = CIImage(cgImage: image).transformed(by: CGAffineTransform(translationX: 24, y: 24))
    }

    func render(_ input: CVPixelBuffer, at time: CMTime, watermarked: Bool) throws -> CMSampleBuffer? {
        var buffer: CVPixelBuffer?
        let allocated = CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(kCFAllocatorDefault, pool, allocation, &buffer)
        // A slow consumer must drop a frame, not grow the pool without a bound.
        if allocated == kCVReturnWouldExceedAllocationThreshold { return nil }
        guard allocated == kCVReturnSuccess, let buffer else { throw CameraWatermarkError.allocationFailed }
        let image = CIImage(cvPixelBuffer: input)
        let scale = min(bounds.width / image.extent.width, bounds.height / image.extent.height)
        let fitted = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let centered = fitted.transformed(by: CGAffineTransform(
            translationX: (bounds.width - fitted.extent.width) / 2 - fitted.extent.minX,
            y: (bounds.height - fitted.extent.height) / 2 - fitted.extent.minY))
        let video = centered.composited(over: background)
        context.render(watermarked ? badge.composited(over: video) : video,
                       to: buffer, bounds: bounds, colorSpace: colorSpace)
        var timing = CMSampleTimingInfo(duration: Self.duration, presentationTimeStamp: time, decodeTimeStamp: .invalid)
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateForImageBuffer(allocator: kCFAllocatorDefault, imageBuffer: buffer,
                                                dataReady: true, makeDataReadyCallback: nil, refcon: nil,
                                                formatDescription: formatDescription, sampleTiming: &timing,
                                                sampleBufferOut: &sample) == noErr else {
            throw CameraWatermarkError.allocationFailed
        }
        return sample
    }

    private static func makeBadge() -> CGImage? {
        let width = 304, height = 52
        guard let canvas = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                                     bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
                                     bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        canvas.setFillColor(CGColor(gray: 0.06, alpha: 0.92))
        canvas.addPath(CGPath(roundedRect: CGRect(x: 0, y: 0, width: CGFloat(width), height: CGFloat(height)), cornerWidth: 10, cornerHeight: 10, transform: nil))
        canvas.fillPath()
        let attributes: [NSAttributedString.Key: Any] = [
            NSAttributedString.Key(kCTFontAttributeName as String): CTFontCreateWithName("Helvetica-Bold" as CFString, 22, nil),
            NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(gray: 1, alpha: 1),
        ]
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: "Recording with Sona", attributes: attributes))
        canvas.textPosition = CGPoint(x: 16, y: 17)
        CTLineDraw(line, canvas)
        return canvas.makeImage()
    }
}
