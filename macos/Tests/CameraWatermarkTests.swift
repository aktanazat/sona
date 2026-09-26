import CoreMedia
import CoreVideo
import XCTest

final class CameraWatermarkTests: XCTestCase {
    func testStoppedPausedOrStaleRecordingCannotKeepLabel() {
        var state = CameraWatermarkState(watermarkEnabled: true, recording: true, cameraID: "test-camera", updatedAt: 100)
        XCTAssertTrue(state.showsNotice(at: 101))
        XCTAssertFalse(state.showsNotice(at: 100 + CameraWatermarkState.lifetime))
        XCTAssertFalse(state.showsNotice(at: 99))
        state.recording = false
        XCTAssertFalse(state.showsNotice(at: 101))
        state.recording = true
        state.watermarkEnabled = false
        XCTAssertFalse(state.showsNotice(at: 101))
    }

    func testRecordingOverlayDisappearsWithoutChangingOtherVideo() throws {
        var created: CVPixelBuffer?
        let result = CVPixelBufferCreate(kCFAllocatorDefault, CameraFrameRenderer.width, CameraFrameRenderer.height,
                                         kCVPixelFormatType_32BGRA,
                                         [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &created)
        XCTAssertEqual(result, kCVReturnSuccess)
        let input = try XCTUnwrap(created)
        CVPixelBufferLockBaseAddress(input, [])
        let pixels = try XCTUnwrap(CVPixelBufferGetBaseAddress(input)).assumingMemoryBound(to: UInt8.self)
        let stride = CVPixelBufferGetBytesPerRow(input)
        for y in 0..<CameraFrameRenderer.height {
            for x in 0..<CameraFrameRenderer.width {
                let offset = y * stride + x * 4
                pixels[offset] = 60; pixels[offset + 1] = 100; pixels[offset + 2] = 140; pixels[offset + 3] = 255
            }
        }
        CVPixelBufferUnlockBaseAddress(input, [])
        let renderer = try CameraFrameRenderer()
        let before = try bytes(XCTUnwrap(renderer.render(input, at: .zero, watermarked: false)))
        let recording = try bytes(XCTUnwrap(renderer.render(input, at: CMTime(value: 1, timescale: 30), watermarked: true)))
        let stopped = try bytes(XCTUnwrap(renderer.render(input, at: CMTime(value: 2, timescale: 30), watermarked: false)))
        XCTAssertEqual(stopped, before, "Stopping must return to the identical physical video, not a black or stale frame.")
        var changedPixels = 0
        var changedOutsideLabel = 0
        for y in 0..<CameraFrameRenderer.height {
            for x in 0..<CameraFrameRenderer.width {
                let offset = (y * CameraFrameRenderer.width + x) * 4
                if recording[offset..<(offset + 4)] != before[offset..<(offset + 4)] {
                    changedPixels += 1
                    // Core Image uses bottom-left coordinates; buffers use top-left.
                    if x < 24 || x >= 328 || y < CameraFrameRenderer.height - 76 || y >= CameraFrameRenderer.height - 24 {
                        changedOutsideLabel += 1
                    }
                }
            }
        }
        XCTAssertGreaterThan(changedPixels, 2_000, "Recording must visibly mark the outgoing frame.")
        XCTAssertEqual(changedOutsideLabel, 0, "The label must not replace, tint or blank the camera video.")
    }

    private func bytes(_ sample: CMSampleBuffer) throws -> [UInt8] {
        let image = try XCTUnwrap(CMSampleBufferGetImageBuffer(sample))
        CVPixelBufferLockBaseAddress(image, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(image, .readOnly) }
        let base = try XCTUnwrap(CVPixelBufferGetBaseAddress(image)).assumingMemoryBound(to: UInt8.self)
        let stride = CVPixelBufferGetBytesPerRow(image)
        var bytes: [UInt8] = []
        bytes.reserveCapacity(CameraFrameRenderer.width * CameraFrameRenderer.height * 4)
        for y in 0..<CameraFrameRenderer.height {
            bytes.append(contentsOf: UnsafeBufferPointer(start: base + y * stride, count: CameraFrameRenderer.width * 4))
        }
        return bytes
    }
}

