import AppKit
import Foundation
import Observation

// MARK: - Request bodies

/// Detection takes the whole struct, deliberately: the core refuses to
/// represent a half-written state such as calendar-on-while-detection-off.
private struct MeetingDetectionWrite: Encodable {
    let settings: MeetingDetectionSettings
}

/// Every fenced write carries the revision it was decided against. The inner
/// body is snake_case; only the parameter around it is camelCase.
private struct MeetingSettingsEnvelope<Body: Encodable>: Encodable {
    let request: Body
}

private struct MeetingRetentionWrite: Encodable {
    let operationId = MeetingSettingsStore.operationId()
    let expectedRevision: Int
    let policy: MeetingRetentionPolicy

    private enum CodingKeys: String, CodingKey {
        case operationId = "operation_id"
        case expectedRevision = "expected_revision"
        case policy
    }
}

private struct MeetingTrackerWrite: Encodable {
    let trackers: [MeetingTracker]
}

private struct MeetingAutomationWrite: Encodable {
    let operationId = MeetingSettingsStore.operationId()
    let seriesKey: String
    let kind: String
    let enabled: Bool
    let target: String?
    let expectedRevision: Int

    private enum CodingKeys: String, CodingKey {
        case operationId = "operation_id"
        case seriesKey = "series_key"
        case kind, enabled, target
        case expectedRevision = "expected_revision"
    }

    /// `target` is written even when it is nothing: `enabled: false` with no
    /// target is what forgets a row, and an omitted key would not say it.
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(operationId, forKey: .operationId)
        try container.encode(seriesKey, forKey: .seriesKey)
        try container.encode(kind, forKey: .kind)
        try container.encode(enabled, forKey: .enabled)
        try container.encode(target, forKey: .target)
        try container.encode(expectedRevision, forKey: .expectedRevision)
    }
}

private struct MeetingSeriesTemplateWrite: Encodable {
    let operationId = MeetingSettingsStore.operationId()
    let seriesKey: String
    let template: MeetingSeriesTemplate?
    let expectedRevision: Int

    private enum CodingKeys: String, CodingKey {
        case operationId = "operation_id"
        case seriesKey = "series_key"
        case template
        case expectedRevision = "expected_revision"
    }

    /// A null template is the mutation that hands the series back to the app
    /// default, so the key is always present.
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(operationId, forKey: .operationId)
        try container.encode(seriesKey, forKey: .seriesKey)
        try container.encode(template, forKey: .template)
        try container.encode(expectedRevision, forKey: .expectedRevision)
    }
}

private struct MeetingSeriesDigestWrite: Encodable {
    let operationId = MeetingSettingsStore.operationId()
    let seriesKey: String
    let digestIncluded: Bool
    let expectedRevision: Int

    private enum CodingKeys: String, CodingKey {
        case operationId = "operation_id"
        case seriesKey = "series_key"
        case digestIncluded = "digest_included"
        case expectedRevision = "expected_revision"
    }
}

/// Granting a standing recording consent names what the operator was shown
/// when they granted it. Revoking acknowledges nothing.
private struct MeetingSeriesAlwaysRecordWrite: Encodable {
    let operationId = MeetingSettingsStore.operationId()
    let seriesKey: String
    let alwaysRecord: Bool
    let policyVersion = 1
    let acknowledgedSources: [String]
    let expectedRevision: Int

    private enum CodingKeys: String, CodingKey {
        case operationId = "operation_id"
        case seriesKey = "series_key"
        case alwaysRecord = "always_record"
        case policyVersion = "policy_version"
        case acknowledgedSources = "acknowledged_sources"
        case expectedRevision = "expected_revision"
    }
}

private struct MeetingRemoteOptOutWrite: Encodable {
    let operationId = MeetingSettingsStore.operationId()
    let seriesKey: String
    let remoteIntelligenceOptOut: Bool
    let expectedRevision: Int

    private enum CodingKeys: String, CodingKey {
        case operationId = "operation_id"
        case seriesKey = "series_key"
        case remoteIntelligenceOptOut = "remote_intelligence_opt_out"
        case expectedRevision = "expected_revision"
    }
}

private struct MeetingRemoteEngineWrite: Encodable {
    let engine: MeetingRemoteEngine
}

private struct MeetingSettingsFlagWrite: Encodable {
    let enabled: Bool
}

private struct MeetingDigestMinuteWrite: Encodable {
    let minuteOfDay: Int
}

private struct MeetingSeriesKeyParam: Encodable {
    let seriesKey: String
}

private struct MeetingSettingsSessionParam: Encodable {
    let sessionId: String
}

// MARK: - The store

/// Everything Advanced ▸ Meetings decides, and the two rows Essentials shows.
///
/// Five independent objects live here because they are read together and
/// nothing else reads them: detection's policy, the app settings subset this
/// page shows, the retention policy, the keyword trackers, and the per-series
/// table behind automations and meeting intelligence. Each keeps its own
/// in-flight flag, because they fail independently and a page-wide spinner
/// would claim otherwise.
@MainActor @Observable final class MeetingSettingsStore {
    private let core: Core

    /// The last thing that went wrong where no row owns the failure.
    private(set) var error: String?

    // Detection.
    private(set) var detection: MeetingDetectionStatus?
    private(set) var detectionSaving = false
    /// macOS answers a decided permission without a dialog, so a refusal is
    /// invisible unless this says it happened. Launch-local on purpose:
    /// System Settings is the only place that can change the answer.
    private(set) var calendarRefused = false
    private(set) var accessAsking = false
    /// The same answer for notifications, which macOS also decides once.
    private(set) var notificationRefused = false
    /// A running-apps reading fresher than the last status. Cleared by the
    /// next status, which carries its own.
    private(set) var runningAppsRefresh: [String]?

    // The app settings subset these rows read.
    private(set) var settings = MeetingSettingsSnapshot()
    private(set) var settingsRead = false
    private(set) var digestSaving = false
    private(set) var remoteSaving = false
    private(set) var engineSaving = false
    private(set) var engineStatus: MeetingRemoteEngineStatus?
    /// The engine the status on screen was read for, so a settings event that
    /// changed something else does not re-ask the endpoint.
    private var engineStatusFor: MeetingRemoteEngine?
    /// The endpoint fields, which are half-typed for most of the time they
    /// exist and so are committed rather than written per keystroke.
    private(set) var endpointBaseUrl = ""
    private(set) var endpointModel = ""
    private(set) var endpointContext = ""
    /// True once the endpoint was picked and before anything is stored for it.
    private(set) var endpointChosen = false
    /// True while a field holds an edit the core has not been told about.
    private(set) var endpointEdited = false
    /// The endpoint the core ships as its default, which is what picking the
    /// endpoint seeds when nothing is stored.
    private var defaultEndpoint: MeetingRemoteEngine?

    // Retention.
    private(set) var retention: MeetingRetentionSnapshot?
    private(set) var retentionSaving = false
    private(set) var retentionNote: String?

    // Keyword trackers.
    private(set) var trackers: [MeetingTracker] = []
    private(set) var trackersRead = false
    private(set) var trackersSaving = false
    private(set) var trackersNote: String?

    // Per-series automations.
    private(set) var roster: MeetingAutomationRoster?
    private(set) var rosterRead = false
    /// The one row being written, as `seriesKey\0kind`.
    private(set) var automationSaving: String?
    private(set) var automationNote: String?
    private(set) var remindersDenied = false
    /// Local edits, keyed by series and kind, so one open field never leaks
    /// into another row when the roster is re-read under it.
    private(set) var automationDrafts: [String: String] = [:]
    private(set) var prompts: [MeetingPromptOption] = []

    // Series that stay on this Mac.
    private(set) var remoteRoster: MeetingRemoteRoster?
    private(set) var remoteRosterRead = false
    private(set) var remoteRowSaving: String?
    private(set) var remoteRowFailed = false

    init(core: Core) {
        self.core = core

        core.observe(CoreEvent.meetingDetectionStatus) { [weak self] line in
            guard let self, let status: MeetingDetectionStatus = try? Core.payload(line) else { return }
            detection = status
            runningAppsRefresh = nil
        }
        // The event carries nothing useful, so the settings are read again.
        core.observe(CoreEvent.meetingSettingsChanged) { [weak self] _ in
            guard let self else { return }
            Task { await self.loadSettings() }
        }
    }

    /// Reads everything this page shows. Independent objects, one wait.
    func start() async {
        async let detection: Void = loadDetection()
        async let settings: Void = loadSettings()
        async let retention: Void = loadRetention()
        async let trackers: Void = loadTrackers()
        async let roster: Void = loadRoster()
        _ = await (detection, settings, retention, trackers, roster)
    }

    /// The identifier a fenced write is retried under. Lowercase, like the
    /// browser's own, because that is what every stored receipt already holds.
    nonisolated static func operationId() -> String { UUID().uuidString.lowercased() }

    // MARK: - Detection

    func loadDetection() async {
        do {
            let status: MeetingDetectionStatus = try await core.request("detection_status_get")
            detection = status
            runningAppsRefresh = nil
        } catch {
            fail(error, "Sona could not load the current meeting data.")
        }
    }

    /// Detection's one write.
    ///
    /// The base is read here rather than supplied by the caller, so a row
    /// holding a stale snapshot cannot send the fields it never touched back
    /// to what they were before the last write; and a write refuses to start
    /// while one is in flight, which is what makes `detectionSaving` an
    /// invariant rather than a convention every row is trusted to honour.
    func patchDetection(_ change: (inout MeetingDetectionSettings) -> Void) async {
        guard var next = detection?.settings, !detectionSaving else { return }
        change(&next)
        detectionSaving = true
        defer { detectionSaving = false }
        do {
            let status: MeetingDetectionStatus = try await core.request(
                "detection_settings_set", MeetingDetectionWrite(settings: next))
            detection = status
            runningAppsRefresh = nil
            error = nil
        } catch {
            fail(error, "Couldn't complete that meeting action. Try again.")
        }
    }

    func setDetectionEnabled(_ enabled: Bool) async {
        await patchDetection { $0.enabled = enabled }
    }

    func setAnyMicActivity(_ enabled: Bool) async {
        await patchDetection { $0.anyMicActivity = enabled }
    }

    /// Turning the calendar path on is what triggers the EventKit request, and
    /// reading events needs full access. Asking first and only writing the
    /// setting on success keeps the switch from claiming a path that cannot
    /// run. The gate is held across the request, because a switch left live
    /// while macOS has its dialog up can be turned off and back on by the
    /// answer.
    func setCalendarEnabled(_ enabled: Bool) async {
        guard enabled else {
            calendarRefused = false
            await patchDetection { $0.calendarEnabled = false }
            return
        }
        guard !detectionSaving else { return }
        accessAsking = true
        let access = await requestCalendarAccess()
        accessAsking = false
        if access == .authorized {
            await patchDetection { $0.calendarEnabled = true }
            calendarRefused = false
        } else {
            // macOS answers a decided permission without a dialog, so the
            // refusal is invisible unless the row says it happened.
            calendarRefused = true
        }
    }

    @discardableResult
    func requestCalendarAccess() async -> MeetingDetectionAccess {
        do {
            let access: MeetingDetectionAccess = try await core.request("detection_calendar_access_request")
            await loadDetection()
            return access
        } catch {
            await loadDetection()
            return .denied
        }
    }

    /// Asking for the notification grant, which is what the prompt a detected
    /// meeting raises needs. The core owns the ask; this row is the only place
    /// the shell offers it.
    func requestNotificationAccess() async {
        guard !accessAsking else { return }
        accessAsking = true
        defer { accessAsking = false }
        do {
            let access: MeetingDetectionAccess = try await core.request("detection_notification_access_request")
            notificationRefused = access != .authorized
            await loadDetection()
            error = nil
        } catch {
            fail(error, "Couldn't complete that meeting action. Try again.")
        }
    }

    /// Which allowlisted apps are running right now. The status carries the
    /// same list and every status replaces this; asking directly is what makes
    /// "Running now" true at the moment the picker is opened rather than at
    /// the last tick.
    func refreshRunningApps() async {
        let running: [String]? = try? await core.request("detection_running_meeting_apps")
        if let running { runningAppsRefresh = running }
    }

    // MARK: - The apps picker

    var detectionSettings: MeetingDetectionSettings? { detection?.settings }

    var meetingApps: [String] { detection?.settings.meetingApps ?? [] }

    var autoRecordApps: [String] { detection?.settings.autoRecordApps ?? [] }

    var runningMeetingApps: [String] { runningAppsRefresh ?? detection?.runningMeetingApps ?? [] }

    /// An entry nobody put behind a name: a renamed vendor identifier, or one
    /// added here. It keeps its own row so removing it does not mean editing a
    /// text blob.
    var customApps: [String] {
        meetingApps.filter { !MeetingAppsCatalog.namedBundleIds.contains($0) }
    }

    func isAppOn(_ entry: MeetingAppsEntry) -> Bool {
        entry.matches.contains { meetingApps.contains($0) }
    }

    func isAppRunning(_ entry: MeetingAppsEntry) -> Bool {
        entry.matches.contains { runningMeetingApps.contains($0) }
    }

    func isAutoRecord(_ entry: MeetingAppsEntry) -> Bool {
        entry.matches.contains { autoRecordApps.contains($0) }
    }

    /// What the summary says, in the order the list is drawn: the products
    /// that are ticked, then whatever was typed.
    var appsSummary: String {
        let listed = MeetingAppsCatalog.known.filter(isAppOn).map(\.name) + customApps
        return listed.isEmpty ? "None" : MeetingSettingsFormat.names(listed)
    }

    func setApp(_ entry: MeetingAppsEntry, on: Bool) async {
        if on {
            await patchDetection { settings in
                settings.meetingApps += entry.writes.filter { !settings.meetingApps.contains($0) }
            }
        } else {
            await dropApps(entry.matches)
        }
    }

    /// Un-listing an app takes its standing grant with it. A grant naming an
    /// app detection no longer watches authorizes nothing, and leaving it
    /// behind would bring auto-recording back the moment the box was ticked
    /// again.
    func dropApps(_ bundleIds: [String]) async {
        await patchDetection { settings in
            settings.meetingApps.removeAll { bundleIds.contains($0) }
            settings.autoRecordApps.removeAll { bundleIds.contains($0) }
        }
    }

    func setAutoRecord(_ entry: MeetingAppsEntry, on: Bool) async {
        await patchDetection { settings in
            if on {
                settings.autoRecordApps += entry.writes.filter { !settings.autoRecordApps.contains($0) }
            } else {
                settings.autoRecordApps.removeAll { entry.matches.contains($0) }
            }
        }
    }

    func addApp(_ bundleId: String) async {
        let entry = bundleId.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard MeetingAppsCatalog.isWellFormed(entry), !meetingApps.contains(entry) else { return }
        await patchDetection { $0.meetingApps.append(entry) }
    }

    /// The identifier of an application bundle the operator picked.
    ///
    /// The web build asked for the reverse-DNS string by hand, because nothing
    /// a webview can reach reads a bundle's Info.plist. A native shell does:
    /// this is the same decision made by pointing at the app.
    func bundleIdentifier(forApplicationAt url: URL) -> String? {
        Bundle(url: url)?.bundleIdentifier?.lowercased()
    }

    /// Where the grant this row needs is changed, for the two rows that can
    /// only tell the operator to go there.
    func openPrivacyPane(_ pane: String) {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?" + pane) else { return }
        NSWorkspace.shared.open(url)
    }

    func openNotificationSettings() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.notifications") else { return }
        NSWorkspace.shared.open(url)
    }

    // MARK: - The settings subset

    func loadSettings() async {
        do {
            let next: MeetingSettingsSnapshot = try await core.request("get_app_settings")
            settings = next
            settingsRead = true
            if defaultEndpoint == nil,
               let defaults: MeetingSettingsSnapshot = try? await core.request("get_default_settings"),
               defaults.localEngine?.isEndpoint == true {
                defaultEndpoint = defaults.localEngine
            }
            syncEndpointDraft()
            await refreshEngineStatus()
        } catch {
            fail(error, "Sona could not load the current meeting data.")
        }
    }

    func setDigestEnabled(_ enabled: Bool) async {
        guard !digestSaving else { return }
        digestSaving = true
        await writeSetting("change_meeting_digest_enabled_setting", MeetingSettingsFlagWrite(enabled: enabled))
        digestSaving = false
    }

    /// The digest time, as minutes past local midnight. A half-typed clock
    /// never gets here: the field converts, and an unconvertible one is not a
    /// write.
    func setDigestMinuteOfDay(_ minuteOfDay: Int) async {
        let clamped = min(max(minuteOfDay, 0), 24 * 60 - 1)
        guard !digestSaving, clamped != settings.digestMinuteOfDay else { return }
        digestSaving = true
        await writeSetting(
            "change_meeting_digest_minute_of_day_setting", MeetingDigestMinuteWrite(minuteOfDay: clamped))
        digestSaving = false
    }

    func setRemoteIntelligenceEnabled(_ enabled: Bool) async {
        guard !remoteSaving else { return }
        remoteSaving = true
        await writeSetting(
            "change_meeting_remote_intelligence_enabled_setting", MeetingSettingsFlagWrite(enabled: enabled))
        remoteSaving = false
        if enabled {
            await loadRemoteRoster()
        } else {
            // The list is only ever about what the switch is doing, so it goes
            // away with it rather than standing as a stale roster.
            remoteRoster = nil
            remoteRosterRead = false
        }
    }

    /// One settings write, then the read that proves it. The core also emits
    /// `settings-changed`, and a second read costs one round trip against a
    /// row that would otherwise show the old value until it arrives.
    private func writeSetting<P: Encodable>(_ method: String, _ params: P) async {
        do {
            try await core.request(method, params)
            await loadSettings()
            error = nil
        } catch {
            fail(error, "Couldn't complete that meeting action. Try again.")
        }
    }

    // MARK: - Where meeting text is written

    var engineIsEndpoint: Bool {
        endpointChosen || settings.localEngine?.isEndpoint == true
    }

    /// The typed context window, or nil for an endpoint that does not state
    /// one. Refuses anything that is not a whole number of tokens.
    private var endpointContextTokens: Int? {
        let trimmed = endpointContext.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? nil : Int(trimmed)
    }

    /// True while the fields hold something the core has not been told. The
    /// status line describes what is stored, so it stands down until this is
    /// false.
    var endpointUnconfigured: Bool {
        guard case let .localEndpoint(baseUrl, model, context) = settings.localEngine else { return endpointChosen }
        return endpointBaseUrl.trimmingCharacters(in: .whitespaces) != baseUrl
            || endpointModel.trimmingCharacters(in: .whitespaces) != model
            || endpointContextTokens != context
    }

    func editEndpointBaseUrl(_ value: String) {
        endpointBaseUrl = value
        endpointEdited = true
    }

    func editEndpointModel(_ value: String) {
        endpointModel = value
        endpointEdited = true
    }

    func editEndpointContext(_ value: String) {
        endpointContext = value
        endpointEdited = true
    }

    func selectEngine(endpoint: Bool) async {
        guard !engineSaving, endpoint != engineIsEndpoint else { return }
        if endpoint {
            endpointChosen = true
            endpointEdited = false
            // Picking the endpoint seeds what is stored for it, or what the
            // core ships as its default. Neither existing leaves the fields
            // empty and nothing written, which is what the status then says.
            let seed = settings.localEngine?.isEndpoint == true ? settings.localEngine : defaultEndpoint
            guard let seed, case let .localEndpoint(baseUrl, model, context) = seed else { return }
            endpointBaseUrl = baseUrl
            endpointModel = model
            endpointContext = context.map(String.init) ?? ""
            await writeEngine(seed)
            return
        }
        endpointChosen = false
        endpointEdited = false
        endpointBaseUrl = ""
        endpointModel = ""
        endpointContext = ""
        await writeEngine(.appleIntelligence)
    }

    /// Commits what is in the endpoint fields. An address is required, a
    /// context window must be a positive whole number, and a value equal to
    /// what is stored is not a write.
    func commitEndpoint() async {
        guard engineIsEndpoint, !engineSaving else { return }
        let baseUrl = endpointBaseUrl.trimmingCharacters(in: .whitespaces)
        guard !baseUrl.isEmpty else { return }
        let model = endpointModel.trimmingCharacters(in: .whitespaces)
        let trimmedContext = endpointContext.trimmingCharacters(in: .whitespaces)
        if !trimmedContext.isEmpty, (endpointContextTokens ?? 0) < 1 { return }
        let next = MeetingRemoteEngine.localEndpoint(
            baseUrl: baseUrl, model: model, contextWindowTokens: endpointContextTokens)
        guard next != settings.localEngine else {
            endpointEdited = false
            return
        }
        await writeEngine(next)
    }

    private func writeEngine(_ engine: MeetingRemoteEngine) async {
        engineSaving = true
        do {
            try await core.request("change_meeting_local_engine_setting", MeetingRemoteEngineWrite(engine: engine))
            endpointEdited = false
            error = nil
        } catch {
            fail(error, "Couldn't complete that meeting action. Try again.")
        }
        engineSaving = false
        await loadSettings()
    }

    /// Re-seeds the fields from what is stored, unless somebody is mid-edit.
    private func syncEndpointDraft() {
        guard !endpointEdited, case let .localEndpoint(baseUrl, model, context) = settings.localEngine else { return }
        endpointBaseUrl = baseUrl
        endpointModel = model
        endpointContext = context.map(String.init) ?? ""
        endpointChosen = false
    }

    /// Asks the chosen engine whether it can be reached. Skipped while the
    /// fields hold an uncommitted edit, because the answer would describe the
    /// address that is stored rather than the one on screen.
    func refreshEngineStatus() async {
        guard !endpointUnconfigured else {
            engineStatus = nil
            engineStatusFor = nil
            return
        }
        guard engineStatusFor != settings.localEngine || engineStatus == nil else { return }
        engineStatusFor = settings.localEngine
        let status: MeetingRemoteEngineStatus? = try? await core.request("meeting_local_engine_status")
        engineStatus = status
    }

    // MARK: - Retention

    func loadRetention() async {
        do {
            let snapshot: MeetingRetentionSnapshot = try await core.request("meeting_retention_get")
            retention = snapshot
            retentionNote = nil
        } catch {
            retention = nil
            retentionNote = Self.sentence(error, "Sona could not load the current meeting data.")
        }
    }

    func setRetention(_ policy: MeetingRetentionPolicy) async {
        guard let snapshot = retention, !retentionSaving, policy != snapshot.policy else { return }
        retentionSaving = true
        retentionNote = nil
        do {
            let mutation: MeetingRetentionMutation = try await core.request(
                "meeting_retention_set",
                MeetingSettingsEnvelope(
                    request: MeetingRetentionWrite(expectedRevision: snapshot.revision, policy: policy)))
            retention = mutation.snapshot
            if mutation.receipt.result != .committed {
                retentionNote = "The meeting action was rejected."
            }
        } catch {
            retentionNote = Self.sentence(error, "Couldn't complete that meeting action. Try again.")
            // The one failure with a next step: somebody else moved the
            // revision, so what is on screen is no longer what is stored.
            if (error as? CoreError)?.remote(as: MeetingSettingsCommandError.self) == .staleRevision {
                await loadRetention()
            }
        }
        retentionSaving = false
    }

    // MARK: - Keyword trackers

    func loadTrackers() async {
        do {
            let saved: [MeetingTracker] = try await core.request("list_keyword_trackers")
            trackers = saved
        } catch {
            trackers = []
        }
        trackersRead = true
    }

    /// Edits one row without saving it: a name is typed a letter at a time,
    /// and every letter is not a write.
    func editTracker(_ index: Int, name: String? = nil, line: String? = nil) {
        guard trackers.indices.contains(index) else { return }
        if let name { trackers[index].name = name }
        if let line { trackers[index].line = line }
    }

    func addTracker() {
        trackers.append(MeetingTracker(name: "", patterns: []))
    }

    func removeTracker(_ index: Int) async {
        guard trackers.indices.contains(index) else { return }
        var next = trackers
        next.remove(at: index)
        await saveTrackers(next)
    }

    func saveTrackers(_ next: [MeetingTracker]? = nil) async {
        guard !trackersSaving else { return }
        let outgoing = next ?? trackers
        trackers = outgoing
        trackersSaving = true
        do {
            let saved: [MeetingTracker] = try await core.request(
                "save_keyword_trackers", MeetingTrackerWrite(trackers: outgoing))
            trackers = saved
            trackersNote = nil
        } catch {
            trackersNote = "Sona could not save the trackers. Try again."
        }
        trackersSaving = false
    }

    // MARK: - Automations

    static func automationKey(_ seriesKey: String, _ kind: MeetingAutomationKind) -> String {
        seriesKey + "\u{0}" + kind.rawValue
    }

    func loadRoster() async {
        do {
            // The prompt list rides along because one kind points at a prompt,
            // and a row offering a picker of identifiers nobody can read is
            // not a picker.
            async let roster: MeetingAutomationRoster = core.request("meeting_automation_roster")
            async let prompts: MeetingPromptList = core.request("saved_prompt_list")
            self.roster = try await roster
            self.prompts = (try? await prompts)?.prompts ?? []
            automationDrafts = [:]
            automationNote = nil
        } catch {
            automationNote = Self.sentence(error, "Sona could not load the current meeting data.")
        }
        rosterRead = true
    }

    /// What a row shows: the local edit if there is one, else what is stored.
    func automationTarget(_ series: MeetingAutomationSeries, _ kind: MeetingAutomationKind) -> String {
        automationDrafts[Self.automationKey(series.seriesKey, kind)] ?? series.target(kind)
    }

    func editAutomationTarget(_ series: MeetingAutomationSeries, _ kind: MeetingAutomationKind, _ value: String) {
        automationDrafts[Self.automationKey(series.seriesKey, kind)] = value
    }

    func isAutomationSaving(_ series: MeetingAutomationSeries, _ kind: MeetingAutomationKind) -> Bool {
        automationSaving == Self.automationKey(series.seriesKey, kind)
    }

    func setAutomation(
        _ series: MeetingAutomationSeries, _ kind: MeetingAutomationKind, enabled: Bool, target: String
    ) async {
        guard let roster, automationSaving == nil else { return }
        let key = Self.automationKey(series.seriesKey, kind)
        automationSaving = key
        automationNote = nil
        do {
            let trimmed = target.trimmingCharacters(in: .whitespacesAndNewlines)
            let result: MeetingAutomationEnableResult = try await core.request(
                "meeting_series_automation_set",
                MeetingSettingsEnvelope(
                    request: MeetingAutomationWrite(
                        seriesKey: series.seriesKey, kind: kind.rawValue, enabled: enabled,
                        target: trimmed.isEmpty ? nil : trimmed, expectedRevision: roster.revision)))
            remindersDenied = kind == .reminders && enabled && result.remindersAccess != .authorized
            if result.mutation.receipt.result == .rejected {
                // The refusal changed nothing, so the honest response is to
                // show what is true now rather than retry behind their back.
                automationNote = "Another window changed these. Read again and retry."
            }
            await loadRoster()
        } catch {
            let remote = (error as? CoreError)?.remote(as: MeetingSettingsCommandError.self)
            automationNote = remote == .invalidRequest
                ? "Sona will not send there. Use localhost or a tailnet address."
                : Self.sentence(error, "That did not save.")
        }
        automationSaving = nil
    }

    /// Every attempt one meeting's automations made, newest first. The review
    /// surface shows these beside the meeting they ran for.
    func automationRuns(sessionId: String) async throws -> [MeetingAutomationRun] {
        try await core.request("meeting_automation_runs", MeetingSettingsSessionParam(sessionId: sessionId))
    }

    /// What one meeting's series has decided, read by session rather than by
    /// key, which is what a meeting open on screen knows about itself.
    func automations(sessionId: String) async throws -> MeetingAutomationSnapshot {
        try await core.request("meeting_series_automations_for_session", MeetingSettingsSessionParam(sessionId: sessionId))
    }

    func automations(seriesKey: String) async throws -> MeetingAutomationSnapshot {
        try await core.request("meeting_series_automations_get", MeetingSeriesKeyParam(seriesKey: seriesKey))
    }

    // MARK: - Series preferences

    func preferences(seriesKey: String) async throws -> MeetingSeriesPreferences {
        try await core.request("meeting_series_template_get", MeetingSeriesKeyParam(seriesKey: seriesKey))
    }

    func preferences(sessionId: String) async throws -> MeetingSeriesPreferences {
        try await core.request("meeting_series_template_for_session", MeetingSettingsSessionParam(sessionId: sessionId))
    }

    @discardableResult
    func setTemplate(seriesKey: String, template: MeetingSeriesTemplate?, revision: Int) async throws
        -> MeetingSeriesMutation
    {
        try await core.request(
            "meeting_series_template_set",
            MeetingSettingsEnvelope(
                request: MeetingSeriesTemplateWrite(
                    seriesKey: seriesKey, template: template, expectedRevision: revision)))
    }

    @discardableResult
    func setDigestIncluded(seriesKey: String, included: Bool, revision: Int) async throws -> MeetingSeriesMutation {
        try await core.request(
            "meeting_series_digest_set",
            MeetingSettingsEnvelope(
                request: MeetingSeriesDigestWrite(
                    seriesKey: seriesKey, digestIncluded: included, expectedRevision: revision)))
    }

    /// A standing grant names the sources it was given for, and it may only
    /// claim what the operator was shown. Revoking acknowledges nothing.
    @discardableResult
    func setAlwaysRecord(seriesKey: String, alwaysRecord: Bool, revision: Int) async throws -> MeetingSeriesMutation {
        try await core.request(
            "meeting_series_always_record_set",
            MeetingSettingsEnvelope(
                request: MeetingSeriesAlwaysRecordWrite(
                    seriesKey: seriesKey, alwaysRecord: alwaysRecord,
                    acknowledgedSources: alwaysRecord ? ["microphone", "system_audio"] : [],
                    expectedRevision: revision)))
    }

    // MARK: - Series that stay on this Mac

    func loadRemoteRoster() async {
        guard settings.remoteIntelligenceEnabled else {
            remoteRoster = nil
            remoteRosterRead = false
            return
        }
        // A roster that cannot be read costs the list, not the switch.
        let roster: MeetingRemoteRoster? = try? await core.request("meeting_series_remote_roster")
        remoteRoster = roster
        remoteRosterRead = true
    }

    func setRemoteOptOut(_ row: MeetingRemoteSeriesRow, optOut: Bool) async {
        guard let roster = remoteRoster, remoteRowSaving == nil else { return }
        remoteRowSaving = row.seriesKey
        remoteRowFailed = false
        do {
            let mutation: MeetingSeriesMutation = try await core.request(
                "meeting_series_remote_opt_out_set",
                MeetingSettingsEnvelope(
                    request: MeetingRemoteOptOutWrite(
                        seriesKey: row.seriesKey, remoteIntelligenceOptOut: optOut,
                        expectedRevision: roster.revision)))
            // The answer carries the receipt and the stored record, so a write
            // another pane fenced out leaves the row showing what is actually
            // stored and the reader can press again.
            remoteRowFailed = mutation.receipt.result != .committed
            await loadRemoteRoster()
        } catch {
            remoteRowFailed = true
        }
        remoteRowSaving = nil
    }

    // MARK: - Failures

    private func fail(_ error: Error, _ fallback: String) {
        self.error = Self.sentence(error, fallback)
    }

    /// A command error the core names itself, said as a sentence; anything
    /// else as the caller's fallback.
    private static func sentence(_ error: Error, _ fallback: String) -> String {
        if let remote = (error as? CoreError)?.remote(as: MeetingSettingsCommandError.self) {
            return remote.sentence
        }
        return (error as? CoreError)?.errorDescription ?? fallback
    }
}
