import Foundation
import Observation

/// The Shared place: where pages go, drafts waiting for him, and every link
/// he has made. The core owns all of it; this holds the last read.
@MainActor
@Observable
final class SharedPagesStore {
    private(set) var site: SharedSite?
    /// Whether the site has been read at all, so "not set up" is never
    /// said before the core has answered.
    private(set) var siteRead = false
    private(set) var drafts: [SharedDraftSummary] = []
    private(set) var shares: [SharedRecord] = []
    private(set) var loaded = false
    private(set) var failure: String?
    private(set) var siteFailure: String?
    private(set) var savingSite = false
    /// The draft or link a press is waiting on.
    private(set) var working: String?

    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
    }

    func start() async {
        await refresh()
    }

    func refresh() async {
        await readSite()
        do {
            let drafts: [SharedDraftSummary] = try await core.request("shared_drafts")
            let shares: [SharedRecord] = try await core.request("shared_list")
            self.drafts = drafts
            self.shares = shares.sorted { ($0.created ?? .distantPast) > ($1.created ?? .distantPast) }
            failure = nil
        } catch {
            failure = SharedFailure.words(error)
        }
        loaded = true
    }

    func readSite() async {
        do {
            let site: SharedSite? = try await core.request("shared_site_get")
            self.site = site
            siteFailure = nil
        } catch {
            siteFailure = SharedFailure.words(error)
        }
        siteRead = true
    }

    func saveSite(remote: String, address: String) async {
        let site = SharedSite(
            remote: remote.trimmingCharacters(in: .whitespacesAndNewlines),
            baseUrl: address.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        savingSite = true
        defer { savingSite = false }
        do {
            let saved: SharedSite? = try await core.request("shared_site_set", CloudSyncRequest(request: site))
            self.site = saved
            siteFailure = nil
        } catch {
            siteFailure = SharedFailure.code(error) == "invalid_draft"
                ? "Check both fields: the repository is a git address and the address starts with https://."
                : SharedFailure.words(error)
        }
    }

    func discard(_ meetingId: String) async {
        working = meetingId
        defer { working = nil }
        do {
            try await core.request("shared_draft_discard", CloudSyncRequest(request: ["meeting_id": meetingId]))
            drafts.removeAll { $0.meetingId == meetingId }
            failure = nil
        } catch {
            failure = SharedFailure.words(error)
        }
    }

    func revoke(_ slug: String) async {
        working = slug
        defer { working = nil }
        do {
            let record: SharedRecord = try await core.request("shared_revoke", CloudSyncRequest(request: ["slug": slug]))
            replace(record)
            failure = nil
        } catch {
            failure = SharedFailure.words(error)
        }
    }

    /// A link the share sheet changed, so the list shows it without a
    /// second read.
    func replace(_ record: SharedRecord) {
        if let index = shares.firstIndex(where: { $0.slug == record.slug }) {
            shares[index] = record
        } else {
            shares.insert(record, at: 0)
        }
    }
}
