import SwiftUI

/// `HeardPlanStatus`.
enum HeardPlanStatus: String, Decodable {
    case heard
    case onCalendar = "on_calendar"
    case happened
    case noRecord = "no_record"

    var symbol: String {
        switch self {
        case .heard: "calendar.badge.plus"
        case .onCalendar: "calendar"
        case .happened: "checkmark.circle"
        case .noRecord: "calendar.badge.clock"
        }
    }
}

struct HeardPlanWindow: Decodable, Hashable {
    let startUtcMs: Int64
    let endUtcMs: Int64
    let allDay: Bool
}

/// `HeardPlan`: a plan to meet, heard in a meeting, kept whatever the answer.
struct HeardPlan: Decodable, Identifiable {
    let planId: String
    let sessionId: MeetingSessionId
    let title: String
    let people: [String]
    let whenText: String
    let window: HeardPlanWindow?
    let place: String?
    let quote: String?
    let citation: ArtifactCitation?
    let status: HeardPlanStatus
    let declined: Bool
    let happenedSessionId: MeetingSessionId?
    var id: String { planId }

    /// A day said with no time has to be given one before it goes on the
    /// calendar.
    var timed: Bool { window.map { !$0.allDay } ?? false }

    var details: String {
        var parts = [when]
        if !people.isEmpty { parts.append(people.joined(separator: ", ")) }
        if let place { parts.append(place) }
        return parts.joined(separator: " · ")
    }

    private var when: String {
        guard let window else { return whenText }
        let start = Date(timeIntervalSince1970: TimeInterval(window.startUtcMs) / 1000)
        let day = Date.FormatStyle.dateTime.weekday(.abbreviated).month(.abbreviated).day()
        return window.allDay ? start.formatted(day) : start.formatted(day.hour().minute())
    }
}

struct HeardPlanEditParams: Encodable {
    struct Request: Encodable {
        let planId: String
        let title: String
        let startUtcMs: Int64
        let endUtcMs: Int64
        let people: [String]
        let place: String?

        enum CodingKeys: String, CodingKey {
            case planId = "plan_id"
            case title
            case startUtcMs = "start_utc_ms"
            case endUtcMs = "end_utc_ms"
            case people
            case place
        }
    }

    let request: Request
}

/// The plans to meet heard in this meeting. Nothing goes on the calendar
/// until the person taps Add, and "Not now" keeps the plan.
struct HeardPlansCard: View {
    let core: Core
    let store: MeetingsStore
    let sessionId: String
    let revision: Int
    @State private var plans: [HeardPlan] = []
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HeardPlanList(core: core, store: store, plans: $plans)
            if let failure { Text(failure).metaText(Theme.live) }
        }
        .padding(.bottom, plans.isEmpty ? 0 : 16)
        .task(id: "\(sessionId):\(revision):\(store.heardPlansRevision)") { await load() }
    }

    private func load() async {
        do {
            let result: [HeardPlan] = try await core.request("meeting_heard_plans", ["sessionId": sessionId])
            guard !Task.isCancelled else { return }
            plans = result
            failure = nil
        } catch {
            guard !Task.isCancelled else { return }
            failure = "Plans heard could not be read."
        }
    }
}

/// Each plan with the line that made it, and what can be done with it. A
/// change here makes every list of the open meeting's plans read them again.
struct HeardPlanList: View {
    let core: Core
    let store: MeetingsStore
    @Binding var plans: [HeardPlan]
    @State private var failure: String?
    @State private var busy = false
    @State private var editing: HeardPlan?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            ForEach(plans) { plan in row(plan) }
            if let failure { Text(failure).metaText(Theme.live) }
        }
        .sheet(item: $editing) { plan in
            HeardPlanEditor(plan: plan) { request in
                let updated: HeardPlan = try await core.request(
                    "meeting_heard_plan_edit", HeardPlanEditParams(request: request))
                changed(updated)
            }
        }
    }

    private func row(_ plan: HeardPlan) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: plan.status.symbol)
                    .foregroundStyle(Theme.inkSecondary)
                    .accessibilityHidden(true)
                Text("Plan heard: \(plan.title)").bodyText(14).lineLimit(2)
                Spacer(minLength: 8)
                if let segmentId = plan.citation?.segmentId {
                    PlaySaidButton(store: store, segmentId: segmentId)
                }
            }
            Text(plan.details).metaText()
            if let quote = plan.quote {
                Text("“\(quote)”").bodyText(13, Theme.inkSecondary).lineLimit(3)
            }
            actions(plan)
        }
    }

    private func actions(_ plan: HeardPlan) -> some View {
        HStack(spacing: 12) {
            switch plan.status {
            case .heard:
                if plan.declined { Text("Not added").metaText() }
                add(plan)
                Button("Edit") { editing = plan }.buttonStyle(.quiet)
                if !plan.declined {
                    Button("Not now") { act(plan, "meeting_heard_plan_not_now") }.buttonStyle(.quiet)
                }
            case .onCalendar:
                Text("On your calendar").metaText()
                Button("Undo") { act(plan, "meeting_heard_plan_undo_calendar") }.buttonStyle(.quiet)
            case .happened:
                Text("Happened").metaText()
                if let later = plan.happenedSessionId {
                    Button("Open the recording") { store.open(later) }.buttonStyle(.quiet)
                }
            case .noRecord:
                Text("No recording of it").metaText()
            }
        }
        .disabled(busy)
    }

    /// After "Not now" the plan stops asking: the way to add it stays, quietly.
    @ViewBuilder
    private func add(_ plan: HeardPlan) -> some View {
        let button = Button(plan.timed ? "Add to calendar" : "Choose a time") {
            if plan.timed { act(plan, "meeting_heard_plan_add_to_calendar") } else { editing = plan }
        }
        if plan.declined { button.buttonStyle(.quiet) } else { button.buttonStyle(.primary) }
    }

    private func act(_ plan: HeardPlan, _ method: String) {
        busy = true
        Task {
            defer { busy = false }
            do {
                let updated: HeardPlan = try await core.request(method, ["planId": plan.planId])
                changed(updated)
                failure = nil
            } catch {
                failure = error.localizedDescription
            }
        }
    }

    private func changed(_ plan: HeardPlan) {
        if let index = plans.firstIndex(where: { $0.planId == plan.planId }) {
            plans[index] = plan
        }
        store.heardPlansChanged()
    }
}

/// ▶ for one line of the open meeting.
struct PlaySaidButton: View {
    let store: MeetingsStore
    let segmentId: TranscriptSegmentId

    var body: some View {
        Button { store.playContribution(segmentId) } label: {
            Image(systemName: "play.fill")
        }
        .buttonStyle(QuietButton(color: Theme.accent, compact: true))
        .disabled(!store.canPlay(segmentId))
        .accessibilityLabel("Play where this was said")
    }
}

/// The plan as the person wants it on the calendar.
struct HeardPlanEditor: View {
    let plan: HeardPlan
    let save: (HeardPlanEditParams.Request) async throws -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State private var start = Date()
    @State private var end = Date()
    @State private var people = ""
    @State private var place = ""
    @State private var saving = false
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Edit plan").headlineText()
            if let quote = plan.quote { Text("“\(quote)”").bodyText(14, Theme.inkSecondary) }
            TextField("Title", text: $title).textFieldStyle(.roundedBorder)
            DatePicker("Starts", selection: $start, displayedComponents: [.date, .hourAndMinute])
            DatePicker("Ends", selection: $end, displayedComponents: [.date, .hourAndMinute])
            TextField("People (separate names with commas)", text: $people).textFieldStyle(.roundedBorder)
            TextField("Location (optional)", text: $place).textFieldStyle(.roundedBorder)
            Text("Uses this Mac's time zone. No invitees are added and no invitations are sent.").metaText()
            if let failure { Text(failure).bodyText(13, Theme.inkSecondary) }
            HStack {
                Button("Cancel") { dismiss() }.buttonStyle(.quiet).keyboardShortcut(.cancelAction)
                Spacer()
                Button(saving ? "Saving…" : "Save") { Task { await submit() } }
                    .buttonStyle(.primary)
                    .disabled(saving || title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || end <= start)
            }
        }
        .padding(28).frame(width: 480).background(Theme.page)
        .interactiveDismissDisabled(saving)
        .onAppear(perform: populate)
    }

    private func populate() {
        title = plan.title
        people = plan.people.joined(separator: ", ")
        place = plan.place ?? ""
        if let window = plan.window, !window.allDay {
            start = Date(timeIntervalSince1970: TimeInterval(window.startUtcMs) / 1000)
            end = Date(timeIntervalSince1970: TimeInterval(window.endUtcMs) / 1000)
            return
        }
        let calendar = Calendar.current
        if let window = plan.window {
            let day = Date(timeIntervalSince1970: TimeInterval(window.startUtcMs) / 1000)
            start = calendar.date(bySettingHour: 9, minute: 0, second: 0, of: day) ?? day
        } else {
            let now = Date()
            start = calendar.nextDate(after: now, matching: DateComponents(minute: 0), matchingPolicy: .nextTime) ?? now
        }
        end = start.addingTimeInterval(3600)
    }

    private func submit() async {
        saving = true
        defer { saving = false }
        let names = people.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        let location = place.trimmingCharacters(in: .whitespacesAndNewlines)
        do {
            try await save(.init(
                planId: plan.planId, title: title,
                startUtcMs: Int64((start.timeIntervalSince1970 * 1000).rounded()),
                endUtcMs: Int64((end.timeIntervalSince1970 * 1000).rounded()),
                people: names, place: location.isEmpty ? nil : location))
            dismiss()
        } catch {
            failure = error.localizedDescription
        }
    }
}
