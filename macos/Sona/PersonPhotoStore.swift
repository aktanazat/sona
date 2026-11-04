import AppKit
import Observation

@MainActor
@Observable
final class PersonPhotoStore {
    struct Photo {
        let image: NSImage?
        let provenance: PersonPhotoProvenance?
        let failure: String?
    }

    private(set) var generation: UInt64 = 0
    @ObservationIgnored private let core: Core
    @ObservationIgnored private let cache = NSCache<NSString, CachedPhoto>()
    @ObservationIgnored private var pending: [String: (UUID, Task<Photo, Never>)] = [:]

    init(core: Core) {
        self.core = core
        cache.countLimit = 64
        cache.totalCostLimit = 64 * 1024 * 1024
        core.observe(CoreEvent.peopleArtifactChanged) { [weak self] _ in self?.invalidate() }
        core.observe(CoreEvent.peopleMeetingRemoved) { [weak self] _ in self?.invalidate() }
    }

    func invalidate() {
        cache.removeAllObjects()
        for (_, task) in pending.values { task.cancel() }
        pending.removeAll()
        generation &+= 1
    }

    /// The photo already read for a person, without waiting on anything.
    func cached(_ personId: String) -> Photo? {
        cache.object(forKey: personId as NSString)?.photo
    }

    func load(_ personId: String) async -> Photo {
        if let cached = cache.object(forKey: personId as NSString) { return cached.photo }
        if let (_, task) = pending[personId] { return await task.value }
        let stamp = generation
        let token = UUID()
        let task = Task<Photo, Never> { [core] in
            do {
                let result: PersonAvatarResult = try await core.request("person_avatar", ["personId": personId])
                guard let avatar = result.avatar else {
                    return Photo(image: nil, provenance: nil, failure: nil)
                }
                let decoded = try await Task.detached(priority: .utility) {
                    guard avatar.pngBase64.utf8.count <= PortraitImage.maximumSavedBytes * 4 / 3 + 4,
                          let bytes = Data(base64Encoded: avatar.pngBase64) else {
                        throw PersonFailure(message: "Couldn't read this photo.")
                    }
                    return try PortraitImage.decode(bytes, maximumEdge: 512)
                }.value
                return Photo(
                    image: NSImage(cgImage: decoded, size: NSSize(width: decoded.width, height: decoded.height)),
                    provenance: avatar.provenance, failure: nil)
            } catch {
                return Photo(image: nil, provenance: nil, failure: PeopleStore.message(error))
            }
        }
        pending[personId] = (token, task)
        let photo = await task.value
        if pending[personId]?.0 == token { pending[personId] = nil }
        guard generation == stamp, !Task.isCancelled else { return photo }
        let cost = photo.image.map { Int($0.size.width * $0.size.height) * 4 } ?? 1
        if photo.failure == nil {
            cache.setObject(CachedPhoto(photo), forKey: personId as NSString, cost: cost)
        }
        return photo
    }

    private final class CachedPhoto {
        let photo: Photo
        init(_ photo: Photo) { self.photo = photo }
    }
}
