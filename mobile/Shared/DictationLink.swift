import Foundation

/// The keyboard's one way back into the app.
///
/// The same scheme is declared under CFBundleURLTypes in the app's Info.plist, which
/// cannot read a constant, so that declaration and this one have to be changed together.
enum DictationLink {
    static let scheme = "sona"
    static let host = "dictate"

    /// Built from the two literals above, so it is a URL by construction.
    static let url = URL(string: "\(scheme)://\(host)")!
    static let meetingURL = URL(string: "\(scheme)://record-meeting")!

    static func keyboardURL(documentID: UUID) -> URL {
        var components = URLComponents(url: url, resolvingAgainstBaseURL: false)!
        components.queryItems = [
            URLQueryItem(name: "keyboard", value: "1"),
            URLQueryItem(name: "document", value: documentID.uuidString),
        ]
        return components.url!
    }

    static func keyboardDocument(_ url: URL) -> UUID? {
        guard opens(url),
              let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems,
              items.contains(where: { $0.name == "keyboard" && $0.value == "1" }),
              let value = items.first(where: { $0.name == "document" })?.value
        else { return nil }
        return UUID(uuidString: value)
    }

    static func opens(_ url: URL) -> Bool {
        url.scheme == scheme && url.host == host
    }
}
