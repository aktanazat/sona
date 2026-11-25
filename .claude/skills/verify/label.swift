// Renders one banner PNG per label segment for record.ts to burn into the
// test video. Reads {"width": N, "banners": [{test, check, result}]} on stdin
// and writes label-000.png, label-001.png, ... into the directory in argv[1].
import AppKit

struct Banner: Decodable {
  let test: String
  let check: String
  let result: String
}

struct Request: Decodable {
  let width: Int
  let banners: [Banner]
}

let outDirectory = URL(fileURLWithPath: CommandLine.arguments[1])
let request = try JSONDecoder().decode(
  Request.self, from: FileHandle.standardInput.readDataToEndOfFile())
let height = 120
let testFont = NSFont.systemFont(ofSize: 26, weight: .regular)
let checkFont = NSFont.systemFont(ofSize: 32, weight: .semibold)

func color(for result: String) -> NSColor {
  switch result {
  case "passed": return .systemGreen
  case "failed": return .systemRed
  case "untested": return .systemOrange
  default: return .white
  }
}

for (index, banner) in request.banners.enumerated() {
  guard
    let bitmap = NSBitmapImageRep(
      bitmapDataPlanes: nil, pixelsWide: request.width, pixelsHigh: height,
      bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
      colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)
  else { fatalError("could not allocate a \(request.width)x\(height) bitmap") }
  NSGraphicsContext.saveGraphicsState()
  NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
  NSColor(white: 0, alpha: 0.8).setFill()
  NSRect(x: 0, y: 0, width: request.width, height: height).fill()
  (banner.test as NSString).draw(
    at: NSPoint(x: 28, y: 72),
    withAttributes: [.font: testFont, .foregroundColor: NSColor(white: 0.85, alpha: 1)])
  (banner.check as NSString).draw(
    at: NSPoint(x: 28, y: 18),
    withAttributes: [.font: checkFont, .foregroundColor: color(for: banner.result)])
  NSGraphicsContext.restoreGraphicsState()
  guard let png = bitmap.representation(using: .png, properties: [:]) else {
    fatalError("could not encode banner \(index)")
  }
  try png.write(to: outDirectory.appendingPathComponent(String(format: "label-%03d.png", index)))
}
