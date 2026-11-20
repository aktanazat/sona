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

    static func keyboardURL(documentID: UUID?, language: TypingLanguage) -> URL {
        var components = URLComponents(url: url, resolvingAgainstBaseURL: false)!
        components.queryItems = [URLQueryItem(name: "keyboard", value: "1")]
            + (documentID.map { [URLQueryItem(name: "document", value: $0.uuidString)] } ?? [])
            + [URLQueryItem(name: "language", value: language.rawValue)]
        return components.url!
    }

    /// The keyboard opens Sona this way only to have the microphone and speech recognition
    /// allowed, which a keyboard cannot ask for itself; nothing is dictated in the app.
    static let keyboardSetupURL = URL(string: "\(scheme)://\(host)?keyboard=1&setup=1")!

    static func keyboardSetup(_ url: URL) -> Bool {
        keyboardQuery("setup", in: url) == "1"
    }

    static func fromKeyboard(_ url: URL) -> Bool {
        opens(url) && URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?
            .contains(where: { $0.name == "keyboard" && $0.value == "1" }) == true
    }

    static func keyboardDocument(_ url: URL) -> UUID? {
        keyboardQuery("document", in: url).flatMap(UUID.init(uuidString:))
    }

    static func keyboardLanguage(_ url: URL) -> TypingLanguage? {
        keyboardQuery("language", in: url).flatMap(TypingLanguage.init(rawValue:))
    }

    private static func keyboardQuery(_ name: String, in url: URL) -> String? {
        guard fromKeyboard(url) else { return nil }
        return URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == name }?.value
    }

    static func opens(_ url: URL) -> Bool {
        url.scheme == scheme && url.host == host
    }
}
