import AppKit
import SwiftUI

/// An app installed on this Mac: the name and icon its own bundle carries.
/// Sona never draws another company's mark; when the app is not installed
/// there is nothing to show.
struct InstalledApp {
    let name: String
    let icon: NSImage

    /// The app LaunchServices finds for `bundleId`, read once per id.
    static func find(_ bundleId: String) -> InstalledApp? {
        let key = bundleId as NSString
        if let found = cache.object(forKey: key) { return found.app }
        let app = read(bundleId)
        cache.setObject(Found(app), forKey: key)
        return app
    }

    private final class Found {
        let app: InstalledApp?
        init(_ app: InstalledApp?) { self.app = app }
    }

    private static let cache = NSCache<NSString, Found>()

    private static func read(_ bundleId: String) -> InstalledApp? {
        let workspace = NSWorkspace.shared
        guard !bundleId.isEmpty,
              let url = workspace.urlForApplication(withBundleIdentifier: bundleId) else { return nil }
        let bundle = Bundle(url: url)
        let name = bundle?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String
            ?? bundle?.object(forInfoDictionaryKey: "CFBundleName") as? String
            ?? url.deletingPathExtension().lastPathComponent
        return InstalledApp(name: name, icon: workspace.icon(forFile: url.path))
    }
}

/// The app something came from, as its own icon: Slack, Zoom, Teams, a
/// browser. An app that is not installed draws `fallback`, an SF Symbol, in
/// secondary ink, or nothing when there is no fallback. VoiceOver reads the
/// app's name; the fallback says nothing a sibling does not.
struct AppMark: View {
    let bundleId: String?
    var fallback: String?
    var size: CGFloat = 16

    var body: some View {
        if let app = bundleId.flatMap(InstalledApp.find) {
            Image(nsImage: app.icon)
                .resizable()
                .interpolation(.high)
                .aspectRatio(contentMode: .fit)
                .frame(width: size, height: size)
                .accessibilityLabel(app.name)
        } else if let fallback {
            Image(systemName: fallback)
                .font(.system(size: size * 0.75, weight: .regular))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: size, height: size)
                .accessibilityHidden(true)
        }
    }
}

/// A line led by the installed app it belongs to, and the line alone when
/// that app is not on this Mac. `announcesApp` speaks the app's name with
/// the line; leave it off where the line already names the app.
struct AppLabel<Title: View>: View {
    let bundleId: String?
    var size: CGFloat = 16
    var announcesApp = false
    @ViewBuilder let title: Title

    var body: some View {
        if let bundleId, InstalledApp.find(bundleId) != nil {
            Label {
                title
            } icon: {
                AppMark(bundleId: bundleId, size: size)
                    .accessibilityHidden(!announcesApp)
            }
            .accessibilityElement(children: .combine)
        } else {
            title
        }
    }
}
