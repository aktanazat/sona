import SwiftUI

extension WorkSubjectKind {
    var noun: String {
        switch self {
        case .project: "project"
        case .initiative: "initiative"
        case .note: "note"
        case .view: "view"
        case .meeting: "meeting"
        case .unknown: "item"
        }
    }

    var icon: String {
        switch self {
        case .project: "folder"
        case .initiative: "scope"
        case .note: "square.and.pencil"
        case .view: "line.3.horizontal.decrease.circle"
        case .meeting: "person.2"
        case .unknown: "questionmark.circle"
        }
    }

    /// What following it brings to the inbox.
    fileprivate var followNews: String {
        switch self {
        case .project: "its updates and status changes"
        case .view: "tasks that join it, finish or are canceled"
        case .note: "agents' edits to it"
        case .initiative, .meeting, .unknown: "what changes"
        }
    }
}

extension AppModel {
    /// A project's page, an initiative's, a note, Tasks on a saved view, or
    /// a meeting's page.
    func openSubject(_ subject: WorkNoticeSubject) {
        switch subject.kind {
        case .project:
            openProject(subject.id)
        case .initiative:
            projects.selectedInitiativeId = subject.id
            go(.initiatives)
        case .note:
            open(link: "sona://note/\(subject.id)")
        case .view:
            go(.tasks)
            workFocus.open(nil, in: .tasks)
            workFocus.switchTab(.view(subject.id))
        case .meeting:
            openMeeting(subject.id)
        case .unknown:
            break
        }
    }
}

/// The bell on a project's, a saved view's or a note's page: following it
/// brings its news to the inbox.
struct FollowButton: View {
    let kind: WorkSubjectKind
    let id: String
    @Environment(AppModel.self) private var model

    var body: some View {
        let on = model.work.isFollowing(kind, id)
        Button {
            model.work.follow(kind, id, !on)
        } label: {
            Label(on ? "Following" : "Follow", systemImage: on ? "bell.fill" : "bell")
        }
        .buttonStyle(.secondary)
        .help(on ? "Stop following" : "Hear about \(kind.followNews) in the inbox")
        .accessibilityValue(on ? "Following" : "Not following")
    }
}

/// "Remind me" on a project, an initiative or a note: the inbox brings it
/// back at the time picked.
struct SubjectReminderMenu: View {
    let subject: WorkSubjectRef
    @Environment(AppModel.self) private var model

    var body: some View {
        let routines = model.routines
        let pending = routines.subjectReminders[subject].flatMap { $0.notifiedAtMs == nil ? $0.remindAtMs : nil }
        let when = pending.map { Self.when($0, now: model.work.now) }
        Menu {
            ForEach(InboxSnooze.allCases, id: \.self) { choice in
                if let at = choice.until {
                    Button(choice.title) { set(at) }
                }
            }
            if pending != nil {
                Divider()
                Button("Clear") { set(nil) }
            }
        } label: {
            Label(when ?? "Remind me", systemImage: "alarm")
        }
        .menuStyle(.button)
        .buttonStyle(.secondary)
        .menuIndicator(.hidden)
        .fixedSize()
        .help(pending.map { "Reminder \(PlanningFormat.date($0))" } ?? "Get a reminder in the inbox")
        .accessibilityLabel(when.map { "Reminder, \($0)" } ?? "Remind me")
    }

    /// "Tomorrow 9:00 AM", "Friday 9:00 AM", "Nov 28 9:00 AM".
    private static func when(_ milliseconds: Int64, now: Int64) -> String {
        let time = PlanningFormat.moment(milliseconds).formatted(date: .omitted, time: .shortened)
        return "\(TaskDates.label(milliseconds, now: now)) \(time)"
    }

    private func set(_ at: Int64?) {
        let routines = model.routines
        let subject = subject
        Task { await routines.setReminder(on: subject, at: at) }
    }
}
