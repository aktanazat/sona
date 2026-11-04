import AppKit
import CoreGraphics
import Foundation

@MainActor
final class QuietReviewScheduler {
    private let store: ProjectsStore
    private let eligible: @MainActor () -> Bool
    private var activity: NSBackgroundActivityScheduler?
    private var revision = 0
    private var earliestRun = Date.distantFuture
    private var work: Task<Void, Never>?
    private var monitor: Task<Void, Never>?
    private var cancellation: Task<Void, Never>?
    private var runningProjectId: String?
    private var lastAttemptedProjectId: String?
    private var sleeping = false
    private var sessionActive = false
    private var paused = false
    private var workspaceObservers: [NSObjectProtocol] = []

    init(store: ProjectsStore, eligible: @escaping @MainActor () -> Bool) {
        self.store = store
        self.eligible = eligible
        sessionActive = Self.onConsole
        let workspace = NSWorkspace.shared.notificationCenter
        for name in [NSWorkspace.willSleepNotification, NSWorkspace.sessionDidResignActiveNotification,
                     NSWorkspace.willPowerOffNotification] {
            workspaceObservers.append(workspace.addObserver(forName: name, object: nil, queue: .main) { [weak self] note in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    if note.name == NSWorkspace.willSleepNotification { self.sleeping = true }
                    if note.name == NSWorkspace.willPowerOffNotification { self.paused = true }
                    self.sessionActive = false
                    self.stop()
                }
            })
        }
        workspaceObservers.append(workspace.addObserver(
            forName: NSWorkspace.didWakeNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.sleeping = false
                self.sessionActive = Self.onConsole
                self.update()
            }
        })
        workspaceObservers.append(workspace.addObserver(
            forName: NSWorkspace.sessionDidBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.sessionActive = Self.onConsole
                self.update()
            }
        })
    }

    func update() {
        guard eligible(), let states = store.quietReviewStates,
              store.quietReviewLoadError == nil,
              !store.suggestionsGenerating, !store.mutating,
              !paused, !sleeping, sessionActive, Self.onConsole else {
            stop()
            return
        }
        if let runningProjectId,
           !states.contains(where: { $0.projectId == runningProjectId && canReview($0) }) {
            stop()
            return
        }
        guard states.contains(where: canReview) else {
            stop()
            return
        }
        guard activity == nil, work == nil, cancellation == nil else { return }
        revision += 1
        let scheduledRevision = revision
        earliestRun = Date.now.addingTimeInterval(15 * 60)
        let scheduler = NSBackgroundActivityScheduler(identifier: "com.aktanazat.sona.mac.quiet-review")
        scheduler.interval = 20 * 60
        scheduler.tolerance = 5 * 60
        scheduler.repeats = true
        scheduler.qualityOfService = .background
        activity = scheduler
        scheduler.schedule { [weak self] completion in
            Task { @MainActor [weak self] in
                guard let self else { completion(.finished); return }
                self.fire(revision: scheduledRevision, completion: completion)
            }
        }
    }

    func setPaused(_ paused: Bool) {
        self.paused = paused
        update()
    }

    private func stop() {
        revision += 1
        activity?.invalidate()
        activity = nil
        work?.cancel()
        monitor?.cancel()
        monitor = nil
        guard let projectId = runningProjectId, cancellation == nil else { return }
        cancellation = Task { [weak self, store] in
            await store.cancelQuietReview(projectId: projectId)
            guard let self else { return }
            self.cancellation = nil
            self.update()
        }
    }

    private func fire(revision scheduledRevision: Int, completion: @escaping NSBackgroundActivityScheduler.CompletionHandler) {
        guard scheduledRevision == revision, work == nil, cancellation == nil,
              let activity, !activity.shouldDefer, canRun else {
            completion(.finished)
            return
        }
        work = Task { [weak self] in
            guard let self else { completion(.finished); return }
            defer {
                self.monitor?.cancel()
                self.monitor = nil
                self.runningProjectId = nil
                self.work = nil
                completion(.finished)
                self.update()
            }
            await self.store.loadQuietReviewGrants()
            guard !Task.isCancelled, scheduledRevision == self.revision, !activity.shouldDefer, self.canRun,
                  let states = self.store.quietReviewStates else { return }
            guard let grant = self.nextGrant(in: states) else { return }
            self.lastAttemptedProjectId = grant.projectId
            self.runningProjectId = grant.projectId
            self.monitor = Task { [weak self] in
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .seconds(5)) }
                    catch { return }
                    guard let self, !Task.isCancelled else { return }
                    if !self.canRun || !self.canReview(grant) || activity.shouldDefer
                        || self.store.quietReview(for: grant.projectId) != grant {
                        self.stop()
                        return
                    }
                }
            }
            await self.store.runQuietReview(projectId: grant.projectId)
        }
    }

    private func canReview(_ grant: QuietReviewState) -> Bool {
        grant.enabled && store.quietReviewSavingProjectId != grant.projectId
            && !store.quietReviewRevocationsPending.contains(grant.projectId)
            && store.projects?.contains(where: { $0.id == grant.projectId && $0.project.status == .active }) == true
    }

    private func nextGrant(in states: [QuietReviewState]) -> QuietReviewState? {
        var first: QuietReviewState?
        var next: QuietReviewState?
        for grant in states where canReview(grant) {
            if first.map({ grant.projectId < $0.projectId }) ?? true { first = grant }
            if let lastAttemptedProjectId, grant.projectId <= lastAttemptedProjectId { continue }
            if next.map({ grant.projectId < $0.projectId }) ?? true { next = grant }
        }
        return next ?? first
    }

    private var canRun: Bool {
        guard !paused, !sleeping, sessionActive, Self.onConsole, eligible(),
              store.quietReviewLoadError == nil,
              !store.suggestionsGenerating, !store.mutating, Date.now >= earliestRun else { return false }
        let hour = Calendar.current.component(.hour, from: .now)
        guard hour >= 22 || hour < 6 else { return false }
        let idle = Self.idleSeconds
        return idle.isFinite && idle >= 10 * 60
    }

    private static var idleSeconds: TimeInterval {
        guard let anyInput = CGEventType(rawValue: UInt32.max) else { return 0 }
        return CGEventSource.secondsSinceLastEventType(.combinedSessionState, eventType: anyInput)
    }

    private static var onConsole: Bool {
        guard let session = CGSessionCopyCurrentDictionary() as? [String: Any],
              let onConsole = session[kCGSessionOnConsoleKey as String] as? Bool,
              let loggedIn = session[kCGSessionLoginDoneKey as String] as? Bool else { return false }
        return onConsole && loggedIn
    }
}
