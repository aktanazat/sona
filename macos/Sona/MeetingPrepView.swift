import SwiftUI

struct MeetingPrepSettingsView: View {
    let store: MeetingPrepStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ErrorNote(store.settingsError)
            if let preferences = store.preferences {
                MeetingPrepForm(store: store, initial: preferences)
            } else if store.preferencesLoading {
                ProgressView("Reading preparation settings…").padding(.vertical, 16)
            } else {
                Button("Read preparation settings again") { Task { await store.loadPreferences() } }
                    .buttonStyle(.secondary)
            }
        }
        .task {
            await store.loadPreferences()
            await store.loadWebConnection()
        }
    }
}

private struct MeetingPrepForm: View {
    let store: MeetingPrepStore
    @State private var draft: MeetingPrepPreferences
    @State private var webKey = ""

    init(store: MeetingPrepStore, initial: MeetingPrepPreferences) {
        self.store = store
        _draft = State(initialValue: initial)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            PageSection("About me") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 12) {
                            Text("Helps Sona choose what matters in notes, briefs and follow-ups. This context goes to your selected meeting model.")
                                .bodyText(13, Theme.inkSecondary)
                            TextField("Name", text: $draft.aboutMe.name).textFieldStyle(.roundedBorder)
                                .accessibilityLabel("Your name")
                            TextField("Role", text: $draft.aboutMe.role).textFieldStyle(.roundedBorder)
                                .accessibilityLabel("Your role")
                            TextField("Company", text: $draft.aboutMe.company).textFieldStyle(.roundedBorder)
                                .accessibilityLabel("Your company")
                            TextField("What should Sona know about your work?", text: $draft.aboutMe.context, axis: .vertical)
                                .lineLimit(3...6).textFieldStyle(.roundedBorder)
                                .accessibilityLabel("Context about your work")
                            Text("Names, role and company: 120 characters each. Context: 1,500 characters.")
                                .metaText(Theme.inkSecondary)
                        }
                    }
                }
            }
            PageSection("Prepare for meetings") {
                Card {
                    ToggleRow(title: "Prepare tomorrow's briefs", detail: "While Sona is running, prepare calendar meetings for today and tomorrow. Nothing is recorded.", isOn: $draft.overnightEnabled)
                    ToggleRow(title: "Include recent email", detail: "Read matching messages from Mail's Inbox and Sent. Email is kept on this Mac and used only by a local model. macOS asks for Automation access.", isOn: $draft.emailContextEnabled)
                    ToggleRow(title: "Open drafts in Mail", detail: "Put the full follow-up in a visible Mail compose window. You review it and send it yourself. Sona never sends mail.", isOn: $draft.mailComposeEnabled)
                    CardRow {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Check that Mail has an account and allows Sona to read it.")
                                .bodyText(13, Theme.inkSecondary)
                            if let status = store.mailStatus { Text(status).bodyText(13, Theme.inkSecondary) }
                        }
                    } trailing: {
                        Button(store.mailChecking ? "Checking…" : "Check Mail") { Task { await store.checkMail() } }
                            .buttonStyle(.compact)
                            .disabled(store.mailChecking || store.preferences?.emailContextEnabled != true || !draft.emailContextEnabled)
                    }
                }
            }
            PageSection("Web research") {
                Card {
                    ToggleRow(
                        title: "Research attendees on the web",
                        detail: "Send attendee names and companies to Brave Search for up to six people per brief. No email addresses, messages, notes or transcripts are sent. Off until you enable it and save these settings.",
                        isOn: $draft.webResearchEnabled)
                    CardRow {
                        VStack(alignment: .leading, spacing: 12) {
                            if store.webConnectionLoading {
                                ProgressView("Reading the saved connection…")
                            } else if let connection = store.webConnection {
                                Text(connection.lastErrorKind?.sentence
                                     ?? (connection.configured ? "A Brave Search key is saved in Keychain." : "No key is saved. Configure Brave Search to include public sources."))
                                    .bodyText(13, Theme.inkSecondary)
                            } else {
                                Button("Read connection again") { Task { await store.loadWebConnection() } }
                                    .buttonStyle(.compact)
                            }
                            if let url = URL(string: "https://api-dashboard.search.brave.com/") {
                                Link("Get a Brave Search API key", destination: url)
                            }
                            SecureField("Brave Search API key", text: $webKey)
                                .textFieldStyle(.roundedBorder)
                                .accessibilityLabel("Brave Search API key")
                            Text("The key needs Web Search access. Testing sends only “Brave Search” and may use a search credit.")
                                .metaText(Theme.inkSecondary)
                            HStack(spacing: 10) {
                                Button("Save key") {
                                    Task {
                                        if await store.saveWebKey(webKey.trimmingCharacters(in: .whitespacesAndNewlines)) {
                                            webKey = ""
                                        }
                                    }
                                }
                                .buttonStyle(.compact)
                                .disabled(webKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                                Button("Test saved key") { Task { await store.testWebConnection() } }
                                    .buttonStyle(.compact)
                                    .disabled(store.webConnection?.configured != true)
                                if store.webConnection?.configured == true {
                                    Button("Remove key") { Task { await store.removeWebKey() } }
                                        .buttonStyle(.quiet)
                                }
                            }
                            .disabled(store.webBusy || store.webConnectionLoading)
                            if store.webBusy { ProgressView("Updating the connection…") }
                            ErrorNote(store.webError)
                            if let notice = store.webNotice { Text(notice).bodyText(13, Theme.inkSecondary) }
                        }
                    }
                }
            }
            PageSection("Meeting reminders") {
                Card {
                    ToggleRow(title: "Upcoming meeting", detail: "Before each calendar meeting, show its brief with a Record button. Needs calendar access above.", isOn: $draft.upcomingAlertsEnabled)
                    CardRow {
                        Stepper("\(draft.alertMinutes) \(draft.alertMinutes == 1 ? "minute" : "minutes") before", value: $draft.alertMinutes, in: 1...60)
                            .accessibilityLabel("Upcoming meeting reminder lead")
                    }
                    ToggleRow(title: "Notes are ready", detail: "Show the saved notes after a recording finishes.", isOn: $draft.notesReadyEnabled)
                    ToggleRow(title: "Unscheduled call", detail: "Offer to record a call that is not on your calendar.", isOn: $draft.adHocAlertsEnabled)
                    ToggleRow(title: "Next meeting in the menu bar", detail: "Show your next meeting or free time. Open the menu to read its brief.", isOn: $draft.menuBarEnabled)
                }
            }
            HStack(spacing: 12) {
                if let notice = store.savedNotice { Text(notice).bodyText(13, Theme.inkSecondary) }
                Spacer(minLength: 12)
                Button(store.saving ? "Saving…" : "Save preparation settings") { Task { await store.save(draft) } }
                    .buttonStyle(.primary)
                    .disabled(store.saving)
            }
            .padding(.bottom, 24)
        }
        .disabled(store.saving)
        .onDisappear { webKey = "" }
    }
}

struct MeetingBriefView: View {
    let store: MeetingPrepStore
    let onClose: () -> Void
    let onOpenMeeting: (MeetingSessionId) -> Void
    let onSettings: () -> Void
    @State private var expandedSources: Set<String> = []

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: 16) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(store.brief?.title ?? "Meeting brief").font(TypeScale.headline).foregroundStyle(Theme.ink)
                    if let brief = store.brief {
                        Text(brief.startUtcMs.meetingDate.formatted(date: .abbreviated, time: .shortened))
                            .metaText(Theme.inkSecondary)
                    }
                }
                Spacer(minLength: 12)
                Button("Close", action: onClose).buttonStyle(.primary)
            }
            .padding(24)
            Hairline()
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 22) {
                        if store.briefLoading {
                            ProgressView("Preparing your brief…")
                        }
                        if let error = store.briefError {
                            ErrorNote(error)
                            Button("Try again") { Task { await store.loadBrief(refresh: true) } }
                                .buttonStyle(.secondary).disabled(store.briefLoading)
                        }
                        if let brief = store.brief {
                            Text(brief.status).bodyText(14, Theme.inkSecondary)
                            points("What to know", brief.content.highlights, brief: brief, proxy: proxy)
                            points("Suggested agenda", brief.content.agenda, brief: brief, proxy: proxy)
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Context used").font(TypeScale.label(15))
                                Text(brief.mail.state.detail).bodyText(13, Theme.inkSecondary)
                                Text(brief.webStatus).bodyText(13, Theme.inkSecondary)
                                Text("Prepared \(brief.generatedAtUtcMs.meetingDate.formatted(date: .abbreviated, time: .shortened)).")
                                    .metaText(Theme.inkSecondary)
                            }
                            VStack(alignment: .leading, spacing: 12) {
                                ForEach(brief.sources) { source in
                                    DisclosureGroup(isExpanded: Binding(
                                        get: { expandedSources.contains(source.id) },
                                        set: { if $0 { expandedSources.insert(source.id) } else { expandedSources.remove(source.id) } }
                                    )) {
                                        VStack(alignment: .leading, spacing: 10) {
                                            Text(source.text).bodyText(13).textSelection(.enabled)
                                            if let value = source.externalUrl, let url = URL(string: value) {
                                                Text("External source. Check that it refers to the right person.")
                                                    .metaText(Theme.inkSecondary)
                                                Link("Open web source", destination: url)
                                                    .accessibilityLabel("Open external source: \(source.label)")
                                            }
                                            if let id = source.meetingId {
                                                Button("Open meeting") { onOpenMeeting(id) }.buttonStyle(.compact)
                                            }
                                        }
                                        .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 8)
                                    } label: {
                                        Text(source.label).bodyText(14)
                                    }
                                    .id(source.id)
                                }
                            }
                        } else if !store.briefLoading && store.briefError == nil {
                            Text("Choose an upcoming meeting to prepare a brief.").bodyText(14, Theme.inkSecondary)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(24)
                }
            }
            Hairline()
            HStack(spacing: 12) {
                Button("Meeting settings", action: onSettings).buttonStyle(.secondary)
                Spacer(minLength: 12)
                Button("Refresh brief") { Task { await store.loadBrief(refresh: true) } }
                    .buttonStyle(.secondary)
                    .disabled(store.briefLoading || store.eventKey == nil)
            }
            .padding(24)
        }
        .frame(width: 660, height: 620)
        .background(Theme.page)
    }

    @ViewBuilder
    private func points(_ title: String, _ points: [MeetingBriefPoint], brief: MeetingBrief, proxy: ScrollViewProxy) -> some View {
        if !points.isEmpty {
            VStack(alignment: .leading, spacing: 14) {
                Text(title).font(TypeScale.label(16))
                ForEach(Array(points.enumerated()), id: \.offset) { _, point in
                    VStack(alignment: .leading, spacing: 6) {
                        Text(point.text).bodyText().textSelection(.enabled)
                        ForEach(point.sources, id: \.self) { id in
                            if let source = brief.sources.first(where: { $0.id == id }) {
                                Button(source.label) {
                                    expandedSources.insert(id)
                                    proxy.scrollTo(id, anchor: .top)
                                }
                                .buttonStyle(.link)
                                .font(TypeScale.label(12))
                                .accessibilityLabel("Read source: \(source.label)")
                            }
                        }
                    }
                }
            }
        }
    }
}
