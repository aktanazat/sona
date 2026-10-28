import Foundation

@MainActor
final class BoundedRewrite {
    enum Outcome: Equatable {
        case rewritten(String)
        case fallback
        case cancelled
    }

    private var request: UUID?
    private var generation: Task<Void, Never>?
    private var deadline: Task<Void, Never>?
    private var continuation: CheckedContinuation<Outcome, Never>?

    func run(
        operation: @escaping @Sendable () async throws -> String,
        waitForDeadline: @escaping @Sendable () async throws -> Void = {
            try await Task.sleep(for: .seconds(8))
        }
    ) async -> Outcome {
        cancel()
        let id = UUID()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                guard !Task.isCancelled else {
                    continuation.resume(returning: .cancelled)
                    return
                }
                request = id
                self.continuation = continuation
                generation = Task { [weak self] in
                    let outcome: Outcome
                    do {
                        let text = try await operation().trimmingCharacters(in: .whitespacesAndNewlines)
                        outcome = text.isEmpty ? .fallback : .rewritten(text)
                    } catch {
                        outcome = .fallback
                    }
                    self?.finish(id: id, outcome: outcome)
                }
                deadline = Task { [weak self] in
                    do { try await waitForDeadline() } catch { return }
                    self?.finish(id: id, outcome: .fallback)
                }
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.finish(id: id, outcome: .cancelled) }
        }
    }

    func cancel() {
        guard let request else { return }
        finish(id: request, outcome: .cancelled)
    }

    private func finish(id: UUID, outcome: Outcome) {
        guard request == id else { return }
        request = nil
        generation?.cancel()
        generation = nil
        deadline?.cancel()
        deadline = nil
        let waiting = continuation
        continuation = nil
        waiting?.resume(returning: outcome)
    }
}
