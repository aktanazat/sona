import Foundation
import Observation

@MainActor
@Observable
final class LoopsStore {
    private(set) var loops: [WorkLoop]?
    private(set) var tools: [WorkLoopTool] = []
    private(set) var selectedId: String?
    private(set) var detail: WorkLoopDetail?
    private(set) var creating = false
    private(set) var busy = false
    private(set) var error: String?

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var poll: Task<Void, Never>?

    init(core: Core) {
        self.core = core
        core.observe("work-changed") { [weak self] _ in
            Task { await self?.refresh() }
        }
    }

    var open: Bool { creating || selectedId != nil }

    func refresh() async {
        await load()
        if selectedId != nil { await loadDetail() }
    }

    func load() async {
        do {
            let listed: WorkLoopList = try await core.request("work_loops")
            loops = Self.sorted(listed.loops)
            tools = listed.tools
        } catch {
            self.error = error.localizedDescription
        }
    }

    func select(_ id: String) {
        stopPolling()
        error = nil
        creating = false
        selectedId = id
        detail = nil
        Task { await loadDetail() }
    }

    func startNew() {
        stopPolling()
        error = nil
        selectedId = nil
        detail = nil
        creating = true
    }

    func close() {
        stopPolling()
        error = nil
        creating = false
        selectedId = nil
        detail = nil
    }

    func loadDetail() async {
        guard let id = selectedId else { return }
        do {
            let loaded: WorkLoopDetail = try await core.request("work_loop", LoopIdParams(loopId: id))
            guard selectedId == id else { return }
            detail = loaded
            replace(loaded.loop)
            schedulePoll()
        } catch {
            guard selectedId == id else { return }
            self.error = error.localizedDescription
        }
    }

    @discardableResult
    func save(_ input: WorkLoopInput) async -> Bool {
        let id = selectedId
        let saved: WorkLoop? = await perform {
            try await self.core.request("work_loop_save", LoopSaveParams(
                operationId: Self.newId(), loopId: id, input: input))
        }
        guard let saved else { return false }
        replace(saved)
        if creating || selectedId == saved.id {
            creating = false
            selectedId = saved.id
            await loadDetail()
        }
        return true
    }

    func run() async {
        guard let id = selectedId else { return }
        let run: WorkLoopRun? = await perform {
            try await self.core.request("work_loop_run", LoopParams(operationId: Self.newId(), loopId: id))
        }
        guard run != nil else { return }
        await loadDetail()
    }

    func setEnabled(_ enabled: Bool) async {
        guard let id = selectedId else { return }
        let saved: WorkLoop? = await perform {
            try await self.core.request("work_loop_enable", LoopEnableParams(
                operationId: Self.newId(), loopId: id, enabled: enabled))
        }
        guard let saved else { return }
        replace(saved)
        await loadDetail()
    }

    func restore(_ version: Int) async {
        guard let id = selectedId else { return }
        let saved: WorkLoop? = await perform {
            try await self.core.request("work_loop_restore_version", LoopRestoreParams(
                operationId: Self.newId(), loopId: id, version: version))
        }
        guard let saved else { return }
        replace(saved)
        await loadDetail()
    }

    func delete() async {
        guard let id = selectedId else { return }
        let deleted: WorkLoop? = await perform {
            try await self.core.request("work_loop_delete", LoopParams(operationId: Self.newId(), loopId: id))
        }
        guard deleted != nil else { return }
        loops?.removeAll { $0.id == id }
        if selectedId == id { close() }
    }

    private func perform<T>(_ call: () async throws -> T) async -> T? {
        busy = true
        error = nil
        defer { busy = false }
        do {
            return try await call()
        } catch {
            self.error = error.localizedDescription
            return nil
        }
    }

    private func replace(_ loop: WorkLoop) {
        guard let current = loops else { return }
        loops = Self.sorted(current.filter { $0.id != loop.id } + [loop])
        if detail?.loop.id == loop.id { detail?.loop = loop }
    }

    private func schedulePoll() {
        guard poll == nil, let detail,
              detail.loop.lastRun?.status.active == true || detail.runs.contains(where: { $0.status.active })
        else { return }
        poll = Task { [weak self] in
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled, let self else { return }
            self.poll = nil
            await self.loadDetail()
        }
    }

    private func stopPolling() {
        poll?.cancel()
        poll = nil
    }

    private static func sorted(_ loops: [WorkLoop]) -> [WorkLoop] {
        loops.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    private static func newId() -> String { UUID().uuidString.lowercased() }
}

private struct LoopIdParams: Encodable { let loopId: String }

private struct LoopParams: Encodable {
    let operationId: String
    let loopId: String
}

private struct LoopSaveParams: Encodable {
    let operationId: String
    let loopId: String?
    let input: WorkLoopInput

    private enum Key: String, CodingKey { case operationId, loopId, input }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        try container.encode(operationId, forKey: .operationId)
        try container.encode(loopId, forKey: .loopId)
        try container.encode(input, forKey: .input)
    }
}

private struct LoopEnableParams: Encodable {
    let operationId: String
    let loopId: String
    let enabled: Bool
}

private struct LoopRestoreParams: Encodable {
    let operationId: String
    let loopId: String
    let version: Int
}
