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

    var body: some View {
        Group {
            if let image = photo?.image {
                Image(nsImage: image).resizable().interpolation(.high).scaledToFill()
                    .transition(.opacity)
            } else if photo?.failure != nil {
                Image(systemName: "photo").foregroundStyle(Theme.inkSecondary)
            } else {
                Text(initials)
                    .font(.system(size: max(11, size * 0.34), weight: .medium))
                    .foregroundStyle(Theme.inkSecondary)
            }
        }
        .frame(width: size, height: size)
        .background(Theme.inset)
        .clipShape(Circle())
        .overlay(Circle().strokeBorder(Theme.border, lineWidth: 1))
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
            .compactMap(\.first).map { String($0).uppercased() }.joined()
    }

    private var accessibilityLabel: String {
        if photo?.failure != nil { return "\(name)'s photo unavailable" }
        return photo?.image == nil ? "\(name)'s initials" : "\(name)'s photo"
    }
}
