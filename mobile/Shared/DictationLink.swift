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

    static func opens(_ url: URL) -> Bool {
        url.scheme == scheme && url.host == host
    }
}
