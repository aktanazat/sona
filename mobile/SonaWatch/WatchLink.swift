import Foundation

/// How the watch face complication opens the watch app.
enum WatchLink {
    /// Opens Sona and starts recording.
    static let record = URL(string: "sona://record")!
}
