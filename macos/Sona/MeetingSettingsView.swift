import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Everything Advanced ▸ Meetings decides, as sections: detection and the apps
/// it watches, what detection can see, what runs after a meeting, where meeting
/// text is written, the evening digest, retention, and the keyword trackers.
///
/// Sections rather than a page, because the integrator owns the page: these
/// drop into Advanced under whatever else it shows, and the two rows Essentials
/// needs are `MeetingDetectionEssentials`.
struct MeetingSettingsView: View {
    let store: MeetingSettingsStore
    /// Where saved prompts are written, which is another surface's business.
    /// The automations rows point at it because a prompt picker with nothing
    /// in it needs somewhere to go.
    var openPrompts: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ErrorNote(store.error)

            PageSection("Detect meetings") {
                Card {
                    MeetingDetectionToggleRow(store: store)
                    MeetingAppsPicker(store: store)
                    MeetingDetectionAdvancedRows(store: store)
                }
            }

            MeetingDetectionStateSection(store: store)
            MeetingAutomationSection(store: store, openPrompts: openPrompts)
            MeetingRemoteSection(store: store)

            PageSection("Evening digest") {
                Card {
                    MeetingDigestRows(store: store)
                }
            }

            PageSection("Retention") {
                Card {
                    MeetingRetentionRows(store: store)
                }
            }

            PageSection("Keyword trackers") {
                Card {
                    MeetingTrackerRows(store: store)
                }
            }
        }
        .task { await store.start() }
    }
}

/// The two rows Essentials shows: the switch that makes detection matter, and
/// the list of what counts as a meeting.
struct MeetingDetectionEssentials: View {
    let store: MeetingSettingsStore

    var body: some View {
        MeetingDetectionToggleRow(store: store)
            // Essentials can be the only surface showing these two rows, so
            // detection is read here as well as by the full settings view.
            .task { await store.loadDetection() }
        MeetingAppsPicker(store: store)
    }
}

// MARK: - Detection

/// The master switch.
///
/// Unread state claims on, because that is the core's own default: rendering
/// "off" would invite a click that turns working detection off.
private struct MeetingDetectionToggleRow: View {
    let store: MeetingSettingsStore

    var body: some View {
        ToggleRow(
            title: "Notice when I join a meeting",
            // The one thing the switch cannot say: noticing is not recording.
            detail: "Sona offers to record. It never starts a recording on its own.",
            isOn: Binding(
                get: { store.detectionSettings?.enabled ?? true },
                set: { on in Task { await store.setDetectionEnabled(on) } }))
        .disabled(store.detectionSettings == nil || store.detectionSaving)
    }
}

/// The calendar path behind its own permission, and the choice that widens
/// what counts as evidence.
private struct MeetingDetectionAdvancedRows: View {
    let store: MeetingSettingsStore

    var body: some View {
        if let settings = store.detectionSettings {
            ToggleRow(
                title: "Use my calendar",
                // The one thing about this switch nobody can infer from it:
                // macOS has no read-only calendar grant, so turning it on asks
                // for the whole calendar.
                detail: "Shows a countdown a minute before events with two or more attendees. "
                    + "macOS asks for full calendar access the first time, because Apple offers no read-only grant.",
                isOn: Binding(
                    get: { settings.calendarEnabled },
                    set: { on in Task { await store.setCalendarEnabled(on) } }))
            .disabled(!settings.enabled || store.detectionSaving || store.accessAsking)

            if store.calendarRefused {
                ActionRow(
                    title: "Calendar access is limited",
                    detail: "macOS is letting Sona add events but not read them, and it only asks once. "
                        + "Choose Full Calendar Access for Sona in System Settings, then turn this on again.",
                    button: "Open System Settings",
                    action: { store.openPrivacyPane("Privacy_Calendars") })
            }

            ToggleRow(
                title: "Ask on any microphone use",
                isOn: Binding(
                    get: { settings.anyMicActivity },
                    set: { on in Task { await store.setAnyMicActivity(on) } }))
            .disabled(!settings.enabled || store.detectionSaving)

            // No open-on-countdown row. `autoStartOnOpenPane` is still on the
            // wire and still round-trips through the whole-struct write, but
            // the consent slice took capture authority away from it in favour
            // of per-series standing consent, so a switch here would claim a
            // decision detection no longer reads.
        } else {
            MeetingSettingsNote("Reading detection state…")
        }
    }
}

/// Silent detection is indistinguishable from broken detection, so every
/// degraded path names itself here, under the one line that always applies:
/// what actually stops a recording.
private struct MeetingDetectionStateSection: View {
    let store: MeetingSettingsStore

    var body: some View {
        if let status = store.detection {
            PageSection("What detection can see") {
                Card {
                    if status.settings.calendarEnabled, status.calendarAccess != .authorized {
                        MeetingSettingsNote(
                            "Calendar access was not granted, so only the microphone path runs.", warning: true)
                    }
                    if status.notificationAccess == .denied {
                        ActionRow(
                            title: "Notifications are off for Sona, so prompts appear in the app only.",
                            button: "Open System Settings",
                            action: { store.openNotificationSettings() })
                    } else if status.notificationAccess == .notDetermined {
                        // The one grant the core can still ask for. React kept
                        // the command and offered it nowhere.
                        ActionRow(
                            title: "Notifications have not been decided, so a detected meeting cannot raise a prompt.",
                            button: "Ask macOS",
                            busy: store.accessAsking,
                            action: { Task { await store.requestNotificationAccess() } })
                    } else if store.notificationRefused {
                        MeetingSettingsNote(
                            "macOS refused notifications for Sona, so prompts appear in the app only.", warning: true)
                    }
                    if status.inputDeviceReportingSuspect {
                        MeetingSettingsNote(
                            "A meeting app is open but nothing reports using the microphone. "
                                + "Bluetooth headsets often do not, so start the meeting yourself if one is running.")
                    }
                    if let reason = status.suppressReason {
                        MeetingSettingsNote(reason.sentence)
                    }
                    if let call = status.adoptedCall {
                        MeetingSettingsNote("This recording stops when the \(call.displayName) call ends.")
                    }
                    MeetingSettingsNote(
                        "Recording stops when the event ends, the call ends, the app quits, your Mac sleeps, "
                            + "or you stop it yourself. Nothing stops it for silence: that would need live "
                            + "transcription, which only runs after a meeting ends.")
                }
            }
        }
    }
}

// MARK: - The apps picker

/// Which applications count as a meeting, as a list of names rather than a
/// text box of reverse-DNS identifiers.
///
/// A disclosure, because this list is set once and then read never. The summary
/// answers the only question a reader brings to it — is the app I call from
/// covered — and the checklist is opened by whoever finds that answer wrong.
private struct MeetingAppsPicker: View {
    let store: MeetingSettingsStore
    @State private var adding = false

    private var blocked: Bool {
        store.detectionSettings == nil || store.detectionSettings?.enabled == false || store.detectionSaving
    }

    var body: some View {
        MeetingSettingsDisclosure(
            label: "Meeting apps",
            fact: store.detectionSettings == nil ? nil : store.appsSummary
        ) {
            ForEach(MeetingAppsCatalog.known) { entry in
                MeetingAppsRow(store: store, entry: entry, blocked: blocked)
            }
            // An entry only ever becomes evidence while that application is
            // running, so the reading is taken whenever the list is opened.
            .task { await store.refreshRunningApps() }
            ForEach(store.customApps, id: \.self) { bundleId in
                MeetingAppsCustomRow(store: store, bundleId: bundleId, blocked: blocked)
            }
            // One sentence, and it is about the switch rather than the list:
            // what "Record automatically" spends is the only thing in here a
            // reader cannot get from the labels. The consent receipt a standing
            // grant writes may only claim what was on screen when it was
            // flipped, and this sentence is that screen.
            MeetingSettingsNote(
                "Record automatically captures your microphone and this Mac's audio output "
                    + "whenever that app is in a call.")
            CardRow {
                Text("Add an app").bodyText()
            } trailing: {
                Button("Add an app") { adding = true }
                    .buttonStyle(.secondary)
                    .disabled(blocked)
            }
        }
        .sheet(isPresented: $adding) {
            MeetingAppsAddSheet(store: store, open: $adding)
        }
    }
}

/// One product the picker offers as a name.
private struct MeetingAppsRow: View {
    let store: MeetingSettingsStore
    let entry: MeetingAppsEntry
    let blocked: Bool

    var body: some View {
        let on = store.isAppOn(entry)
        CardRow {
            HStack(spacing: 10) {
                Toggle("", isOn: Binding(get: { on }, set: { next in Task { await store.setApp(entry, on: next) } }))
                    .labelsHidden()
                    .toggleStyle(.checkbox)
                    .disabled(blocked)
                Text(entry.name).bodyText()
                if store.isAppRunning(entry) {
                    Text("Running now").metaText()
                }
            }
        } trailing: {
            // The switch belongs only to the two call apps: the core reads a
            // standing grant for a call signal alone, so a grant stored for
            // Zoom would round-trip, read back as on, and record nothing.
            if entry.isCall {
                HStack(spacing: 10) {
                    Text("Record automatically").metaText(Theme.inkSecondary)
                    Toggle(
                        "",
                        isOn: Binding(
                            get: { store.isAutoRecord(entry) },
                            set: { next in Task { await store.setAutoRecord(entry, on: next) } })
                    )
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .tint(Theme.accent)
                    .disabled(blocked || !on)
                }
            }
        }
    }
}

/// An entry nobody put behind a name: a renamed vendor identifier, or one
/// added here. It keeps its own row so removing it does not mean editing a
/// text blob.
private struct MeetingAppsCustomRow: View {
    let store: MeetingSettingsStore
    let bundleId: String
    let blocked: Bool

    var body: some View {
        CardRow {
            HStack(spacing: 10) {
                Toggle("", isOn: Binding(get: { true }, set: { _ in Task { await store.dropApps([bundleId]) } }))
                    .labelsHidden()
                    .toggleStyle(.checkbox)
                    .disabled(blocked)
                Text(bundleId).bodyText()
                if store.runningMeetingApps.contains(bundleId) {
                    Text("Running now").metaText()
                }
            }
        } trailing: {
            Button("Remove") { Task { await store.dropApps([bundleId]) } }
                .buttonStyle(.quiet)
                .disabled(blocked)
        }
    }
}

/// Adding an app.
///
/// The web build could only take a typed identifier: nothing a webview reaches
/// reads a bundle's Info.plist, and a picker that returned a path and then
/// still demanded the identifier would be an affordance that does not do its
/// job. A native shell reads the bundle, so pointing at the app is the same
/// decision with none of the typing. The field stays for the app that is not
/// installed here.
private struct MeetingAppsAddSheet: View {
    let store: MeetingSettingsStore
    @Binding var open: Bool
    @State private var draft = ""

    private var bundleId: String {
        draft.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }

    private var problem: String? {
        if !bundleId.isEmpty, !MeetingAppsCatalog.isWellFormed(bundleId) {
            return "An app ID looks like com.example.app: lowercase words separated by dots."
        }
        if store.meetingApps.contains(bundleId), !bundleId.isEmpty {
            return "That app is already on the list."
        }
        return nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Add a meeting app").headlineText()
            Text("Point at the application, or type its ID if it is not installed on this Mac.")
                .bodyText(14, Theme.inkSecondary)

            HStack(spacing: 10) {
                MeetingSettingsField(prompt: "com.example.app", text: $draft, width: 260, commit: {})
                Button("Choose app…") { choose() }
                    .buttonStyle(.secondary)
            }

            if let problem {
                Text(problem).bodyText(14, Theme.live)
            }

            HStack(spacing: 10) {
                Spacer()
                Button("Cancel") { close() }
                    .buttonStyle(.secondary)
                Button("Add") { add() }
                    .buttonStyle(.primary)
                    .disabled(bundleId.isEmpty || problem != nil)
            }
        }
        .padding(24)
        .frame(width: 520)
        .background(Theme.page)
    }

    private func choose() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.application]
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.directoryURL = URL(fileURLWithPath: "/Applications")
        panel.prompt = "Choose"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        // An application Sona cannot read an identifier out of is one detection
        // could never match, so the field says so rather than storing the path.
        draft = store.bundleIdentifier(forApplicationAt: url) ?? ""
    }

    private func add() {
        let entry = bundleId
        guard !entry.isEmpty, problem == nil else { return }
        Task { await store.addApp(entry) }
        close()
    }

    private func close() {
        draft = ""
        open = false
    }
}

// MARK: - After a meeting

/// What a recorded series does once its notes are written.
private struct MeetingAutomationSection: View {
    let store: MeetingSettingsStore
    var openPrompts: () -> Void = {}

    var body: some View {
        PageSection("After a meeting") {
            Card {
                if !store.rosterRead {
                    MeetingSettingsNote("Reading your series…")
                } else if store.roster?.series.isEmpty ?? true {
                    MeetingSettingsNote("Record a meeting from a calendar event to see its automations here.")
                } else {
                    MeetingSettingsNote("Actions that run on this Mac when a series' notes are written.")
                }

                if let roster = store.roster {
                    ForEach(roster.series) { series in
                        MeetingAutomationSeriesRows(store: store, series: series)
                    }
                }

                if store.remindersDenied {
                    MeetingSettingsNote(
                        "macOS has not granted access to Reminders. Open Privacy & Security ▸ Reminders to change that.",
                        warning: true)
                }
                if let note = store.automationNote {
                    MeetingSettingsNote(note, tone: Theme.live)
                }

                // One kind points at a saved prompt, and those are written on
                // their own surface. The count is what says whether the picker
                // has anything in it.
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Saved prompts").bodyText()
                        Text(store.prompts.isEmpty
                            ? "None yet. A saved prompt is what \"Run a saved prompt\" runs."
                            : "\(store.prompts.count) saved, and any of them can run after a meeting.")
                            .metaText()
                    }
                } trailing: {
                    Button("Open") { openPrompts() }
                        .buttonStyle(.secondary)
                }
            }
        }
    }
}

/// One series, and the four things it can be told to do.
private struct MeetingAutomationSeriesRows: View {
    let store: MeetingSettingsStore
    let series: MeetingAutomationSeries

    var body: some View {
        MeetingSettingsDisclosure(
            label: series.title.isEmpty ? series.seriesKey : series.title,
            fact: "\(series.meetingCount) recorded · last met \(MeetingSettingsFormat.day(series.lastMetAtUtcMs))"
        ) {
            ForEach(MeetingAutomationKind.allCases) { kind in
                MeetingAutomationRow(store: store, series: series, kind: kind)
            }
        }
    }
}

private struct MeetingAutomationRow: View {
    let store: MeetingSettingsStore
    let series: MeetingAutomationSeries
    let kind: MeetingAutomationKind

    var body: some View {
        let target = store.automationTarget(series, kind)
        let enabled = series.isEnabled(kind)
        let busy = store.isAutomationSaving(series, kind)
        // An empty target is refused by the core, so the switch says why
        // rather than sending a write that cannot land.
        let blocked = kind.placeholder != nil && target.trimmingCharacters(in: .whitespaces).isEmpty

        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(kind.label).bodyText()
                Text(kind.hint).metaText()
                if blocked, !enabled, let note = kind.blockedNote {
                    Text(note).metaText(Theme.accent)
                }
            }
        } trailing: {
            HStack(spacing: 10) {
                if kind == .runPrompt {
                    // A pick is a decision, so it writes on the spot — unlike
                    // a URL, which is half-typed for most of the time it exists.
                    Picker(
                        "",
                        selection: Binding(
                            get: { target },
                            set: { next in
                                Task { await store.setAutomation(series, kind, enabled: enabled, target: next) }
                            })
                    ) {
                        if target.isEmpty {
                            Text("Pick a prompt").tag("")
                        }
                        ForEach(store.prompts) { prompt in
                            Text(prompt.name).tag(prompt.promptId)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .fixedSize()
                    .disabled(busy || store.prompts.isEmpty)
                } else if let placeholder = kind.placeholder {
                    MeetingSettingsField(
                        prompt: placeholder,
                        text: Binding(
                            get: { target },
                            set: { store.editAutomationTarget(series, kind, $0) }),
                        width: 224,
                        disabled: busy,
                        commit: {
                            guard !blocked, target != series.target(kind) else { return }
                            Task { await store.setAutomation(series, kind, enabled: enabled, target: target) }
                        })
                }
                Toggle(
                    "",
                    isOn: Binding(
                        get: { enabled },
                        set: { next in
                            Task { await store.setAutomation(series, kind, enabled: next, target: target) }
                        })
                )
                .labelsHidden()
                .toggleStyle(.switch)
                .tint(Theme.accent)
                .disabled(busy || (blocked && !enabled))
            }
        }
    }
}

// MARK: - Meeting intelligence

/// Where a meeting's summaries, ledgers, recaps and answers are written, the
/// switch that sends them to the operator's own server, and the series that
/// never leave this Mac.
private struct MeetingRemoteSection: View {
    let store: MeetingSettingsStore

    /// What the endpoint currently answers, or what is keeping it from being
    /// asked.
    private var engineLine: String {
        guard store.endpointUnconfigured else {
            return store.engineStatus?.sentence ?? "Checking local engine…"
        }
        return store.settings.localEngine?.isEndpoint == true
            ? "Not saved yet. Leave the field, or press Enter, to save it."
            : "Local endpoint is not configured yet."
    }

    private var engineLineIsWarning: Bool {
        store.endpointUnconfigured || store.engineStatus?.isWarning == true
    }

    var body: some View {
        PageSection("Meeting intelligence") {
            Card {
                ChoiceRow(
                    title: "Write meeting text on",
                    detail: "Choose where meeting summaries, ledgers, recaps and answers are generated.",
                    choices: [false, true],
                    label: { $0 ? "Local OpenAI-compatible endpoint" : "Apple Intelligence" },
                    selection: Binding(
                        get: { store.engineIsEndpoint },
                        set: { endpoint in Task { await store.selectEngine(endpoint: endpoint) } }))
                .disabled(store.engineSaving)

                if store.engineIsEndpoint {
                    MeetingRemoteEndpointRows(store: store)
                }

                // The status line describes what is stored, so while the
                // fields hold something the core has not been told it says so
                // instead. "Not saved" and "never configured" are different
                // problems with different fixes.
                MeetingSettingsNote(engineLine, warning: engineLineIsWarning)

                ToggleRow(
                    title: "Write meeting notes on my server",
                    isOn: Binding(
                        get: { store.settings.remoteIntelligenceEnabled },
                        set: { on in Task { await store.setRemoteIntelligenceEnabled(on) } }))
                .disabled(!store.settings.isRelayPaired || store.remoteSaving)

                MeetingSettingsNote(
                    "Summaries and answers for meetings are written on your server over your private network.")
                if !store.settings.isRelayPaired {
                    MeetingSettingsNote("Pair Sona with your server under Agents to turn this on.", warning: true)
                }

                if store.settings.remoteIntelligenceEnabled {
                    MeetingRemoteSeriesRows(store: store)
                }
            }
        }
    }
}

/// The three fields an endpoint needs, committed when they are left rather
/// than per keystroke.
private struct MeetingRemoteEndpointRows: View {
    let store: MeetingSettingsStore

    var body: some View {
        MeetingSettingsFieldRow(
            title: "Endpoint address",
            detail: "Loopback only. Include the /v1 path.",
            prompt: "http://127.0.0.1:11434/v1",
            text: Binding(get: { store.endpointBaseUrl }, set: { store.editEndpointBaseUrl($0) }),
            disabled: store.engineSaving,
            commit: { Task { await store.commitEndpoint() } })

        MeetingSettingsFieldRow(
            title: "Model",
            detail: "Name a model from the endpoint's /v1/models list.",
            prompt: "For example, llama3.2",
            text: Binding(get: { store.endpointModel }, set: { store.editEndpointModel($0) }),
            disabled: store.engineSaving,
            commit: { Task { await store.commitEndpoint() } })

        MeetingSettingsFieldRow(
            title: "Context window (tokens)",
            detail: "Required when /v1/models does not report a context size.",
            prompt: "For example, 8192",
            text: Binding(get: { store.endpointContext }, set: { store.editEndpointContext($0) }),
            disabled: store.engineSaving,
            commit: { Task { await store.commitEndpoint() } })
    }
}

/// A series listed here is always written on this Mac, even while meeting
/// intelligence is on.
private struct MeetingRemoteSeriesRows: View {
    let store: MeetingSettingsStore

    var body: some View {
        MeetingSettingsDisclosure(
            label: "Series that stay on this Mac",
            fact: store.remoteRoster.map { roster in
                let kept = roster.rows.filter(\.remoteIntelligenceOptOut)
                return kept.isEmpty ? "None" : MeetingSettingsFormat.names(kept.map(\.title), total: kept.count)
            }
        ) {
            MeetingSettingsNote(
                "A series listed here is always written on this Mac, even while meeting intelligence is on.")
            if let roster = store.remoteRoster {
                if roster.rows.isEmpty {
                    MeetingSettingsNote("No recurring meetings recorded yet.")
                }
                ForEach(roster.rows) { row in
                    ToggleRow(
                        title: row.title.isEmpty ? row.seriesKey : row.title,
                        detail: "Last met \(MeetingSettingsFormat.day(row.lastMetAtUtcMs)) · \(row.meetings) recorded",
                        isOn: Binding(
                            get: { row.remoteIntelligenceOptOut },
                            set: { next in Task { await store.setRemoteOptOut(row, optOut: next) } }))
                    .disabled(store.remoteRowSaving != nil)
                }
            } else {
                MeetingSettingsNote("Reading your series…")
            }
            if store.remoteRowFailed {
                MeetingSettingsNote(
                    "That change did not save. Read the row again and try once more.", tone: Theme.live)
            }
        }
        .task { await store.loadRemoteRoster() }
    }
}

// MARK: - Digest

/// One notification at the end of a day that had meetings or closed loops.
private struct MeetingDigestRows: View {
    let store: MeetingSettingsStore
    @State private var clock = ""

    var body: some View {
        ToggleRow(
            title: "Evening digest",
            detail: "One notification at the end of a day that had meetings or closed loops. "
                + "Everything stays on this Mac.",
            isOn: Binding(
                get: { store.settings.digestEnabled },
                set: { on in Task { await store.setDigestEnabled(on) } }))
        .disabled(store.digestSaving)
        // The stored minute is the field's truth: it seeds the text and
        // replaces it again whenever the core reports a different one.
        .task(id: store.settings.digestMinuteOfDay) {
            clock = MeetingDigestClock.text(store.settings.digestMinuteOfDay)
        }

        if store.settings.digestEnabled {
            MeetingSettingsFieldRow(
                title: "Digest time",
                detail: "Local time. A day with nothing in it stays quiet.",
                prompt: "18:00",
                text: $clock,
                width: 96,
                disabled: store.digestSaving,
                commit: commit)
        }
    }

    /// A half-typed clock is not a time, and the stored one is what it goes
    /// back to.
    private func commit() {
        guard let minute = MeetingDigestClock.minuteOfDay(clock) else {
            clock = MeetingDigestClock.text(store.settings.digestMinuteOfDay)
            return
        }
        Task { await store.setDigestMinuteOfDay(minute) }
    }
}

// MARK: - Retention

/// Sona deletes meetings on this Mac the same verified way every time.
private struct MeetingRetentionRows: View {
    let store: MeetingSettingsStore

    private var choices: [MeetingRetentionPolicy] {
        [.forever] + MeetingRetentionPolicy.dayChoices.map { .deleteAfter(days: $0) }
    }

    var body: some View {
        if let snapshot = store.retention {
            ChoiceRow(
                title: "Retention",
                // The one thing the control cannot state: what "delete" means.
                detail: "Sona deletes meetings on this Mac the same verified way every time.",
                choices: choices,
                label: { $0.label },
                selection: Binding(
                    get: { snapshot.policy },
                    set: { policy in Task { await store.setRetention(policy) } }))
            .disabled(store.retentionSaving)
        } else {
            MeetingSettingsNote("Reading the current policy…")
        }

        if let note = store.retentionNote {
            ActionRow(
                title: note,
                button: "Try again",
                busy: store.retentionSaving,
                action: { Task { await store.loadRetention() } })
        }
    }
}

// MARK: - Keyword trackers

/// Watch lists for words that matter. Every finished transcript is scanned for
/// them on this Mac, and the hits show on the meeting's own Insights tab.
private struct MeetingTrackerRows: View {
    let store: MeetingSettingsStore

    /// The names, which are what a person recognises, and a count that covers
    /// every row. A tracker being edited has no name yet, so a roster of only
    /// blank rows falls back to its size rather than claiming there is nothing
    /// here.
    private var fact: String {
        let named = store.trackers.map { $0.name.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        if store.trackers.isEmpty { return "None" }
        if named.isEmpty { return String(store.trackers.count) }
        return MeetingSettingsFormat.names(named, total: store.trackers.count)
    }

    var body: some View {
        MeetingSettingsDisclosure(label: "Keyword trackers", fact: store.trackersRead ? fact : nil) {
            if store.trackers.isEmpty {
                MeetingSettingsNote("Add a tracker to start counting how often a phrase comes up.")
            }
            ForEach(Array(store.trackers.enumerated()), id: \.offset) { index, tracker in
                CardRow {
                    HStack(spacing: 10) {
                        MeetingSettingsField(
                            prompt: "Name",
                            text: Binding(
                                get: { tracker.name },
                                set: { store.editTracker(index, name: $0) }),
                            width: 150,
                            disabled: store.trackersSaving,
                            commit: { Task { await store.saveTrackers() } })
                        MeetingSettingsField(
                            prompt: "discount, best price, too expensive",
                            text: Binding(
                                get: { tracker.line },
                                set: { store.editTracker(index, line: $0) }),
                            width: 260,
                            disabled: store.trackersSaving,
                            commit: { Task { await store.saveTrackers() } })
                    }
                } trailing: {
                    Button("Remove") { Task { await store.removeTracker(index) } }
                        .buttonStyle(.quiet)
                        .disabled(store.trackersSaving)
                }
            }
            if let note = store.trackersNote {
                MeetingSettingsNote(note, tone: Theme.live)
            }
            CardRow {
                Text("Phrases are literal, not patterns: \"is that your best price?\" is a phrase somebody says.")
                    .metaText()
            } trailing: {
                Button("Add tracker") { store.addTracker() }
                    .buttonStyle(.secondary)
                    .disabled(store.trackersSaving)
            }
        }
    }
}

// MARK: - Shared pieces

/// A row that opens: a label, what it currently says, and the rows underneath
/// once somebody asks. What a setting is set to belongs beside its name, so the
/// closed row answers the question most readers bring to it.
private struct MeetingSettingsDisclosure<Content: View>: View {
    let label: String
    var fact: String?
    // No open handler: a reading taken when the list opens is `.task` on the
    // content, which only exists while it is open. A second closure here would
    // also sit between the caller and its trailing content closure.
    @ViewBuilder var content: () -> Content
    @State private var open = false

    var body: some View {
        CardRow(action: {
            open.toggle()
        }) {
            HStack(spacing: 8) {
                Image(systemName: open ? "chevron.down" : "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
                Text(label).bodyText()
            }
        } trailing: {
            if let fact {
                Text(fact).metaText(fact == "None" ? Theme.accent : Theme.inkTertiary)
            }
        }
        if open {
            content()
        }
    }
}

/// One line of standing fact inside a card: what detection can see, what a
/// switch spends, what a list would say if it had anything in it.
private struct MeetingSettingsNote: View {
    let text: String
    var tone: Color

    init(_ text: String, warning: Bool = false, tone: Color? = nil) {
        self.text = text
        self.tone = tone ?? (warning ? Theme.accent : Theme.inkTertiary)
    }

    var body: some View {
        CardRow {
            Text(text).bodyText(14, tone)
        }
    }
}

/// A labelled row whose control is one field.
private struct MeetingSettingsFieldRow: View {
    let title: String
    var detail: String?
    let prompt: String
    @Binding var text: String
    var width: CGFloat = 224
    var disabled = false
    let commit: () -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).bodyText()
                if let detail {
                    Text(detail).metaText()
                }
            }
        } trailing: {
            MeetingSettingsField(prompt: prompt, text: $text, width: width, disabled: disabled, commit: commit)
        }
    }
}

/// A text field that commits when it is left, or on Enter.
///
/// `InputField` carries this exact treatment but keeps its text field to
/// itself, and commit-on-blur needs the field's own focus: a typed address is
/// half-written for most of the time it exists, so writing per keystroke would
/// send a dozen invalid endpoints for every good one.
private struct MeetingSettingsField: View {
    let prompt: String
    @Binding var text: String
    var width: CGFloat = 224
    var disabled = false
    let commit: () -> Void
    @FocusState private var focused: Bool

    var body: some View {
        TextField(prompt, text: $text, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary))
            .textFieldStyle(.plain)
            .font(TypeScale.body())
            .foregroundStyle(Theme.ink)
            .focused($focused)
            .onSubmit { focused = false }
            .onChange(of: focused) { _, isFocused in
                if !isFocused { commit() }
            }
            .padding(.horizontal, 12)
            .frame(width: width, height: 36)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
            .disabled(disabled)
    }
}

// MARK: - One meeting's series

/// What the series behind one meeting has decided, for the review page to show
/// beside the meeting itself.
///
/// Separate from the settings sections above because this is a decision about
/// one series, reached from the meeting that belongs to it, and the revision it
/// writes against is that series' own. A meeting in no series renders nothing:
/// there is nothing to remember it against, and a disabled row would be the app
/// asking a question it already knows the answer to.
///
/// MeetingsReview: `MeetingSeriesSection(store:sessionId:)` with the open
/// meeting's id, inside its page.
struct MeetingSeriesSection: View {
    let store: MeetingSettingsStore
    let sessionId: String
    /// Where the automations themselves are edited.
    var openMeetingSettings: () -> Void = {}

    @State private var preferences: MeetingSeriesPreferences?
    @State private var automations: MeetingAutomationSnapshot?
    @State private var runs: [MeetingAutomationRun] = []
    @State private var saving = false
    @State private var note: String?

    private var rules: [MeetingAutomationRule] {
        (automations?.automations ?? []).filter { $0.enabled && $0.kind != nil }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let preferences, preferences.seriesKey != nil {
                PageSection("This series") {
                    Card {
                        ChoiceRow(
                            title: "Notes template",
                            detail: "Every meeting in this series is written this way.",
                            choices: [nil] + MeetingSeriesTemplate.allCases.map { Optional($0) },
                            label: { $0?.label ?? "App default" },
                            selection: Binding(
                                get: { preferences.template },
                                set: { template in
                                    write { revision in
                                        try await store.setTemplate(
                                            seriesKey: preferences.seriesKey ?? "",
                                            template: template,
                                            revision: revision)
                                    }
                                }))
                        .disabled(saving)

                        ToggleRow(
                            title: "Include in the evening digest",
                            isOn: Binding(
                                get: { preferences.digestIncluded },
                                set: { included in
                                    write { revision in
                                        try await store.setDigestIncluded(
                                            seriesKey: preferences.seriesKey ?? "",
                                            included: included,
                                            revision: revision)
                                    }
                                }))
                        .disabled(saving)

                        ToggleRow(
                            title: "Record this series automatically",
                            // What the switch spends, on the screen it is
                            // flipped from: the receipt it writes may only
                            // claim what the operator could read here.
                            detail: "Sona captures your microphone and this Mac's audio output "
                                + "when a meeting in this series starts, without asking.",
                            isOn: Binding(
                                get: { preferences.alwaysRecord },
                                set: { always in
                                    write { revision in
                                        try await store.setAlwaysRecord(
                                            seriesKey: preferences.seriesKey ?? "",
                                            alwaysRecord: always,
                                            revision: revision)
                                    }
                                }))
                        .disabled(saving)

                        if let note {
                            MeetingSettingsNote(note, tone: Theme.live)
                        }

                        CardRow {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("Automations").bodyText()
                                Text(rules.isEmpty
                                    ? "Nothing runs after a meeting in this series."
                                    : MeetingSettingsFormat.names(rules.compactMap { $0.kind?.label }))
                                    .metaText()
                            }
                        } trailing: {
                            Button("Edit") { openMeetingSettings() }
                                .buttonStyle(.secondary)
                        }
                    }
                }
            }

            if !runs.isEmpty {
                PageSection("What ran after this meeting") {
                    Card {
                        ForEach(runs) { run in
                            MeetingSeriesRunRow(run: run)
                        }
                    }
                }
            }
        }
        .task(id: sessionId) { await load() }
    }

    /// Three independent reads, one wait. Each costs only its own rows.
    private func load() async {
        async let preferences = store.preferences(sessionId: sessionId)
        async let automations = store.automations(sessionId: sessionId)
        async let runs = store.automationRuns(sessionId: sessionId)
        self.preferences = try? await preferences
        self.automations = try? await automations
        self.runs = (try? await runs) ?? []
        note = nil
    }

    /// Every write carries the revision this view is looking at. A write the
    /// core fences out leaves the rows showing what is actually stored, which
    /// is what the re-read is for.
    private func write(_ change: @escaping (Int) async throws -> MeetingSeriesMutation) {
        guard let current = preferences, !saving else { return }
        saving = true
        note = nil
        Task {
            do {
                preferences = try await change(current.revision).preferences
            } catch {
                note = (error as? CoreError)?.remote(as: MeetingSettingsCommandError.self)?.sentence
                    ?? "That change did not save."
                await load()
            }
            saving = false
        }
    }
}

/// One attempt, and how far it got. Receipts are kept forever, so a row that
/// says nothing ran is as much of an answer as one that says it did.
private struct MeetingSeriesRunRow: View {
    let run: MeetingAutomationRun

    private var sentence: String {
        switch run.state {
        case .committed: run.effects > 0 ? "Ran · \(run.effects) written" : "Ran"
        case .failed: run.failure ?? "Failed"
        case .started: "Started and never reported back"
        }
    }

    var body: some View {
        let started = Date(timeIntervalSince1970: Double(run.startedAtUtcMs) / 1000)
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(run.kind?.label ?? "Unknown automation").bodyText()
                Text(sentence).metaText(run.state == .committed ? Theme.inkTertiary : Theme.accent)
                if let detail = run.detail {
                    Text(detail).metaText()
                }
            }
        } trailing: {
            Text("\(started.short) \(started.time)").metaText()
        }
    }
}
