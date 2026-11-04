import SwiftUI

struct PersonPortrait: View {
    let personId: String
    let name: String
    let size: CGFloat
    let revision: UInt64

    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var loaded: Loaded?
    @State private var reload: UInt64 = 0

    /// A photo is drawn only beside the person it was read for, so a portrait
    /// moved to somebody else never shows the last face.
    private struct Loaded {
        let personId: String
        let photo: PersonPhotoStore.Photo
    }

    /// The last read for this person, or what the photo store already holds.
    /// A re-read keeps the face up until its answer lands.
    private var photo: PersonPhotoStore.Photo? {
        if let loaded, loaded.personId == personId { return loaded.photo }
        return model.personPhotos.cached(personId)
    }

    private var failed: Bool { photo?.image == nil && photo?.failure != nil }

    var body: some View {
        Group {
            if let image = photo?.image {
                Image(nsImage: image).resizable().interpolation(.high).scaledToFill()
                    .transition(.opacity)
            } else if failed {
                Image(systemName: "photo").foregroundStyle(Theme.inkSecondary)
            } else if initials.isEmpty {
                Image(systemName: "person.fill")
                    .font(.system(size: size * 0.5))
                    .foregroundStyle(.white)
            } else {
                Text(initials)
                    .font(.system(size: size * 0.4, weight: .semibold))
                    .foregroundStyle(.white)
                    .lineLimit(1)
                    .minimumScaleFactor(0.6)
                    .padding(.horizontal, size * 0.08)
            }
        }
        .frame(width: size, height: size)
        .background {
            if failed {
                Theme.inset
            } else {
                MonogramFill(color: PersonInk.color(for: personId))
            }
        }
        .clipShape(Circle())
        .overlay(Circle().strokeBorder(Theme.border, lineWidth: 1).opacity(photo?.image == nil && !failed ? 0 : 1))
        .accessibilityLabel(accessibilityLabel)
        .help(photo?.provenance?.label ?? photo?.failure ?? accessibilityLabel)
        .contextMenu {
            if let provenance = photo?.provenance {
                Text(provenance.label)
                Text("Confirmed \(provenance.confirmedAt.formatted(date: .abbreviated, time: .omitted))")
                if let url = provenance.sourceURL { Link("Open source", destination: url) }
            } else if photo?.image != nil {
                Text("Source not recorded")
            }
            if photo?.failure != nil {
                Button("Retry photo") { reload &+= 1 }
            }
        }
        .task(id: "\(personId):\(revision):\(model.personPhotos.generation):\(reload)") {
            let shown = photo?.image != nil
            let fresh = await model.personPhotos.load(personId)
            guard !Task.isCancelled else { return }
            withAnimation(reduceMotion || shown || fresh.image == nil ? nil : .easeOut(duration: 0.18)) {
                loaded = Loaded(personId: personId, photo: fresh)
            }
        }
    }

    private var initials: String {
        name.split(whereSeparator: \.isWhitespace).prefix(2)
            .compactMap { $0.first(where: \.isLetter) }.map { String($0).uppercased() }.joined()
    }

    private var accessibilityLabel: String {
        if failed { return "\(name)'s photo unavailable" }
        return photo?.image == nil ? "\(name)'s initials" : "\(name)'s photo"
    }
}

/// A person's own colour, chosen from their id alone, so they wear the same
/// one on every screen and after every launch. Each colour keeps white text
/// above 4.5:1 in either appearance.
enum PersonInk {
    private static let palette: [Color] = [
        0x1D5FC0, 0xA84A0A, 0x1C7139, 0xB3265A, 0x136A78, 0x7A3E8F, 0xA8382B, 0x565E6B,
    ].map { (hex: UInt32) in
        Color(
            .sRGB,
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }

    static func color(for personId: String) -> Color {
        var hash: UInt64 = 0xCBF2_9CE4_8422_2325
        for byte in personId.utf8 {
            hash ^= UInt64(byte)
            hash &*= 0x0000_0100_0000_01B3
        }
        return palette[Int(hash % UInt64(palette.count))]
    }
}

/// A monogram's ground: the colour, deepening a little toward the bottom.
/// The shade only darkens, so white on it never falls below the flat colour.
struct MonogramFill: View {
    let color: Color

    var body: some View {
        color.overlay {
            LinearGradient(colors: [.clear, .black.opacity(0.14)], startPoint: .top, endPoint: .bottom)
        }
    }
}
