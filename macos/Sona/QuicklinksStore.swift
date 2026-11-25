import Foundation
import Observation

struct Quicklink: Identifiable, Codable, Sendable {
    let id: UUID
    let name: String
    let address: String
    private let components: URLComponents
    let host: String
    private let hasArgument: Bool

    private static let argument = "{argument}"
    private static let encodedArgument = "%7Bargument%7D"
    private static let unreserved = CharacterSet(charactersIn:
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")

    init(id: UUID = UUID(), name: String, address: String) throws {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { throw QuicklinkError.nameRequired }
        let address = address.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !address.contains("\\"), address.rangeOfCharacter(from: .controlCharacters) == nil,
              let parts = URLComponents(string: address.replacingOccurrences(
                of: Self.argument, with: Self.encodedArgument)),
              let scheme = parts.scheme?.lowercased(), scheme == "https" || scheme == "http",
              let host = parts.host, !host.isEmpty,
              parts.user == nil, parts.password == nil, parts.url != nil else {
            throw QuicklinkError.invalidAddress
        }
        let occurrences = address.components(separatedBy: Self.argument).count - 1
        if occurrences != 0 {
            let beforeFragment = address.utf8.prefix { $0 != 0x23 }
            let query = beforeFragment.split(separator: 0x3F, maxSplits: 1).dropFirst().first
            let inValues = query?.split(separator: 0x26).reduce(into: 0) { count, pair in
                if let equal = pair.firstIndex(of: 0x3D) {
                    let value = String(decoding: pair.suffix(from: pair.index(after: equal)), as: UTF8.self)
                    count += value.components(separatedBy: Self.argument).count - 1
                }
            } ?? 0
            guard inValues == occurrences else { throw QuicklinkError.argumentOutsideValue }
        }
        self.id = id
        self.name = name
        self.address = address
        self.host = host
        components = parts
        hasArgument = occurrences != 0
    }

    private enum Keys: String, CodingKey { case id, name, address }

    init(from decoder: Decoder) throws {
        let box = try decoder.container(keyedBy: Keys.self)
        try self.init(id: box.decode(UUID.self, forKey: .id),
                      name: box.decode(String.self, forKey: .name),
                      address: box.decode(String.self, forKey: .address))
    }

    func encode(to encoder: Encoder) throws {
        var box = encoder.container(keyedBy: Keys.self)
        try box.encode(id, forKey: .id)
        try box.encode(name, forKey: .name)
        try box.encode(address, forKey: .address)
    }

    var needsArgument: Bool { hasArgument }

    func destination(argument: String? = nil) throws -> URL {
        guard hasArgument else {
            guard let url = components.url else { throw QuicklinkError.invalidAddress }
            return url
        }
        guard let argument, !argument.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              let value = argument.addingPercentEncoding(withAllowedCharacters: Self.unreserved) else {
            throw QuicklinkError.argumentRequired
        }
        let resolved = address.replacingOccurrences(of: Self.argument, with: value)
        guard let url = URLComponents(string: resolved)?.url else { throw QuicklinkError.invalidAddress }
        return url
    }
}

enum QuicklinkError: LocalizedError {
    case nameRequired, invalidAddress, argumentOutsideValue, argumentRequired
    case duplicateName, changed, unavailable, saveFailed, cannotOpen

    var errorDescription: String? {
        switch self {
        case .nameRequired: "Enter a name."
        case .invalidAddress: "Enter a full web address without a username or password."
        case .argumentOutsideValue: "Put {argument} in a query value, such as ?q={argument}."
        case .argumentRequired: "Enter a search value."
        case .duplicateName: "A quicklink already uses that name."
        case .changed: "This quicklink changed. Close the editor and open it again."
        case .unavailable: "Quicklinks couldn’t be read. Try again."
        case .saveFailed: "Quicklinks couldn’t be saved. Try again."
        case .cannotOpen: "The browser couldn’t open this link."
        }
    }
}

struct QuicklinkFile: Sendable {
    let url: URL

    func read() throws -> [Quicklink] {
        do {
            let data = try Data(contentsOf: url)
            let links = try JSONDecoder().decode([Quicklink].self, from: data)
            guard Set(links.map(\.id)).count == links.count else { throw QuicklinkError.unavailable }
            return links
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            return []
        }
    }

    func write(_ links: [Quicklink]) throws {
        do {
            try JSONEncoder().encode(links).write(to: url, options: .atomic)
        } catch {
            throw QuicklinkError.saveFailed
        }
    }
}

@MainActor
@Observable
final class QuicklinksStore {
    enum State {
        case waiting, loading, unavailable
        case ready(QuicklinkFile, [Quicklink])
    }

    private(set) var state = State.waiting
    var openError: String?
    private let core: Core

    init(core: Core) {
        self.core = core
    }

    var links: [Quicklink] {
        if case let .ready(_, links) = state { return links }
        return []
    }

    func load() async {
        switch state {
        case .waiting, .unavailable: break
        case .loading, .ready: return
        }
        state = .loading
        do {
            let path: String = try await core.request("get_app_dir_path")
            let file = QuicklinkFile(url: URL(fileURLWithPath: path, isDirectory: true)
                .appendingPathComponent("quicklinks.json"))
            let links = try await Task.detached { try file.read() }.value
            state = .ready(file, links)
        } catch {
            state = .unavailable
        }
    }

    func save(name: String, address: String, replacing original: Quicklink?) throws {
        guard case let .ready(file, current) = state else { throw QuicklinkError.unavailable }
        let entry = try Quicklink(id: original?.id ?? UUID(), name: name, address: address)
        guard !current.contains(where: {
            $0.id != entry.id && $0.name.compare(entry.name, options: [.caseInsensitive, .diacriticInsensitive]) == .orderedSame
        }) else { throw QuicklinkError.duplicateName }
        var updated = current
        if let original {
            guard let index = current.firstIndex(where: {
                $0.id == original.id && $0.name == original.name && $0.address == original.address
            }) else { throw QuicklinkError.changed }
            updated[index] = entry
        } else {
            updated.append(entry)
        }
        try file.write(updated)
        state = .ready(file, updated)
    }

    func remove(_ id: UUID) throws {
        guard case let .ready(file, current) = state else { throw QuicklinkError.unavailable }
        guard let index = current.firstIndex(where: { $0.id == id }) else { throw QuicklinkError.changed }
        var updated = current
        updated.remove(at: index)
        try file.write(updated)
        state = .ready(file, updated)
    }
}
