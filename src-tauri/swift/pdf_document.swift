import AppKit
import Foundation
import PDFKit

// A PDF read and marked on this Mac with PDFKit.
//
// The core hands over a PDF's bytes and gets back the text of each page, or
// hands over the bytes with the passages to mark and gets back the bytes of a
// marked copy. The bytes handed over are never changed. Everything here is
// private except the C entry points, because each Swift bridge is compiled on
// its own and linked into the same binary.

private enum PdfResult: Int32 {
    case ok = 0
    case invalidArgument = 1
    case unreadable = 2
    case writeFailed = 3
}

/// One passage to highlight on a page counted from 0, with the note that
/// goes beside it.
private struct PdfMark: Decodable {
    let page: Int
    let passage: String
    let note: String
}

private let pdfNoteSize: CGFloat = 18

private func pdfDocument(_ bytes: UnsafePointer<UInt8>?, _ length: UInt) -> PDFDocument? {
    guard let bytes, length > 0 else { return nil }
    guard let document = PDFDocument(data: Data(bytes: bytes, count: Int(length))),
          !document.isLocked else {
        return nil
    }
    return document
}

private func pdfCopy(_ bytes: Data, into out: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>,
                     length outLength: UnsafeMutablePointer<UInt>) -> Bool {
    guard let buffer = malloc(max(bytes.count, 1))?.assumingMemoryBound(to: UInt8.self) else {
        return false
    }
    bytes.copyBytes(to: buffer, count: bytes.count)
    out.pointee = buffer
    outLength.pointee = UInt(bytes.count)
    return true
}

/// The highlight on every line of the passage and one note beside its first
/// line, all carrying the note. False when the passage is not on the page.
private func pdfMark(_ mark: PdfMark, in document: PDFDocument) -> Bool {
    guard mark.page < document.pageCount, let page = document.page(at: mark.page),
          let text = page.string else { return false }
    let range = (text as NSString).range(of: mark.passage)
    guard range.location != NSNotFound, let selection = page.selection(for: range) else { return false }
    let lines = selection.selectionsByLine()
        .map { $0.bounds(for: page) }
        .filter { !$0.isEmpty }
    guard let first = lines.first else { return false }
    for line in lines {
        let highlight = PDFAnnotation(bounds: line, forType: .highlight, withProperties: nil)
        highlight.color = NSColor.systemYellow.withAlphaComponent(0.5)
        highlight.contents = mark.note
        page.addAnnotation(highlight)
    }
    let media = page.bounds(for: .mediaBox)
    let note = PDFAnnotation(
        bounds: CGRect(
            x: min(first.maxX + 4, media.maxX - pdfNoteSize),
            y: first.maxY - pdfNoteSize,
            width: pdfNoteSize,
            height: pdfNoteSize),
        forType: .text,
        withProperties: nil)
    note.contents = mark.note
    note.color = NSColor.systemYellow
    page.addAnnotation(note)
    return true
}

/// The notes in a written copy that carry one of `notes`: what the copy
/// holds, read back rather than assumed.
private func pdfNotes(in data: Data, matching notes: Set<String>) -> UInt32? {
    guard let document = PDFDocument(data: data) else { return nil }
    var count: UInt32 = 0
    for index in 0..<document.pageCount {
        guard let page = document.page(at: index) else { continue }
        for annotation in page.annotations
        where annotation.type == "Text" && annotation.contents.map(notes.contains) == true {
            count += 1
        }
    }
    return count
}

/// The text of every page, in order, as a JSON array of strings. A page
/// with no text is an empty string.
@_cdecl("sona_pdf_page_texts")
public func sonaPdfPageTexts(
    _ bytes: UnsafePointer<UInt8>?,
    _ length: UInt,
    _ outJSON: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    guard let outJSON else { return PdfResult.invalidArgument.rawValue }
    outJSON.pointee = nil
    guard let document = pdfDocument(bytes, length) else { return PdfResult.unreadable.rawValue }
    let pages = (0..<document.pageCount).map { document.page(at: $0)?.string ?? "" }
    guard let json = try? JSONEncoder().encode(pages),
          let text = String(data: json, encoding: .utf8),
          let copy = strdup(text) else {
        return PdfResult.unreadable.rawValue
    }
    outJSON.pointee = copy
    return PdfResult.ok.rawValue
}

/// A copy of the PDF with each mark's passage highlighted and its note
/// beside it, as bytes, and how many notes the copy holds when read back.
/// A passage no longer on its page is left unmarked.
@_cdecl("sona_pdf_marked_copy")
public func sonaPdfMarkedCopy(
    _ bytes: UnsafePointer<UInt8>?,
    _ length: UInt,
    _ marksJSON: UnsafePointer<CChar>?,
    _ outBytes: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outLength: UnsafeMutablePointer<UInt>?,
    _ outMarked: UnsafeMutablePointer<UInt32>?
) -> Int32 {
    guard let marksJSON, let outBytes, let outLength, let outMarked,
          let marks = try? JSONDecoder().decode([PdfMark].self, from: Data(String(cString: marksJSON).utf8))
    else {
        return PdfResult.invalidArgument.rawValue
    }
    outBytes.pointee = nil
    outLength.pointee = 0
    outMarked.pointee = 0
    guard let document = pdfDocument(bytes, length) else { return PdfResult.unreadable.rawValue }
    let placed = marks.filter { pdfMark($0, in: document) }
    guard let data = document.dataRepresentation(),
          let marked = pdfNotes(in: data, matching: Set(placed.map(\.note))),
          pdfCopy(data, into: outBytes, length: outLength) else {
        return PdfResult.writeFailed.rawValue
    }
    outMarked.pointee = marked
    return PdfResult.ok.rawValue
}

@_cdecl("sona_pdf_free")
public func sonaPdfFree(_ pointer: UnsafeMutableRawPointer?) {
    free(pointer)
}
