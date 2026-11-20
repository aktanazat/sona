import SwiftUI

extension WorkSubjectKind {
    var noun: String {
        switch self {
        case .project: "project"
        case .initiative: "initiative"
        case .note: "note"
        case .view: "view"
        case .unknown: "item"
        }
    }

    var icon: String {
        switch self {
        case .project: "folder"
        case .initiative: "scope"
        case .note: "square.and.pencil"
        case .view: "line.3.horizontal.decrease.circle"
        case .unknown: "questionmark.circle"
        }
    }

    /// What following it brings to the inbox.
    fileprivate var followNews: String {
        switch self {
        case .project: "its updates and status changes"
        case .view: "tasks that join it, finish or are canceled"
        case .note: "agents' edits to it"
        case .initiative, .unknown: "what changes"
        }
    }
}

extension AppModel {
    /// A project's page, an initiative's, a note, or Tasks on a saved view.
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
        .help(on
            ? "Following: \(kind.followNews) reach the inbox. Click to stop."
            : "Follow this \(kind.noun): \(kind.followNews) reach the inbox.")
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
        let reminder = routines.subjectReminders[subject]
        Menu {
            ForEach(InboxSnooze.allCases, id: \.self) { choice in
                if let at = choice.until {
                    Button(choice.title) { set(at) }
                }
            }
            if reminder != nil {
                Divider()
                Button("Clear") { set(nil) }
            }
        } label: {
            Label(reminder.map(Self.title) ?? "Remind me", systemImage: "alarm")
        }
        .menuStyle(.button)
        .buttonStyle(.secondary)
        .menuIndicator(.hidden)
        .fixedSize()
        .help(reminder == nil ? "Get a reminder in the inbox" : "Change or clear the reminder")
    }

    private static func title(_ reminder: WorkSubjectReminder) -> String {
        let when = PlanningFormat.date(reminder.remindAtMs)
        return reminder.notifiedAtMs == nil ? "Remind \(when)" : "Reminded \(when)"
    }

    private func set(_ at: Int64?) {
        let routines = model.routines
        let subject = subject
        Task { await routines.setReminder(on: subject, at: at) }
    }
}
