import SwiftUI

/// ?: every key the task screens answer to.
struct ShortcutSheet: View {
    @Environment(AppModel.self) private var model

    private struct Entry: Identifiable {
        let title: String
        let keys: [String]
        var id: String { title }
    }

    private static let columns: [[(title: String, entries: [Entry])]] = [
        [
            ("Go to", [
                Entry(title: "Home", keys: ["g h"]),
                Entry(title: "Today", keys: ["g d"]),
                Entry(title: "Meetings", keys: ["g m"]),
                Entry(title: "Tasks", keys: ["g t"]),
                Entry(title: "Triage", keys: ["g i"]),
                Entry(title: "People", keys: ["g p"]),
                Entry(title: "Projects", keys: ["g j"]),
            ]),
            ("View", [
                Entry(title: "New task", keys: ["c"]),
                Entry(title: "Filter", keys: ["f"]),
                Entry(title: "Display options", keys: ["shift+v"]),
                Entry(title: "List or board", keys: ["cmd+b"]),
                Entry(title: "Command menu", keys: ["cmd+k"]),
                Entry(title: "Shortcuts", keys: ["?"]),
            ]),
            ("Shutdown", [
                Entry(title: "Tomorrow", keys: ["right"]),
                Entry(title: "This week", keys: ["w"]),
                Entry(title: "Drop", keys: ["x"]),
            ]),
        ],
        [
            ("List", [
                Entry(title: "Next and previous", keys: ["j", "k"]),
                Entry(title: "Move the highlight", keys: ["down", "up"]),
                Entry(title: "Select", keys: ["x"]),
                Entry(title: "Select a range", keys: ["shift+down", "shift+click"]),
                Entry(title: "Select all", keys: ["cmd+a"]),
                Entry(title: "Peek", keys: ["space"]),
                Entry(title: "Open", keys: ["return"]),
                Entry(title: "Back, clear", keys: ["esc"]),
            ]),
            ("Triage", [
                Entry(title: "Accept", keys: ["1"]),
                Entry(title: "Decline", keys: ["2"]),
                Entry(title: "Duplicate", keys: ["3"]),
                Entry(title: "Next or previous source", keys: ["tab", "shift+tab"]),
            ]),
            ("Today", [
                Entry(title: "Pick a focus", keys: ["space"]),
                Entry(title: "Start the day", keys: ["return"]),
                Entry(title: "Still true: keep", keys: ["k"]),
                Entry(title: "Still true: archive", keys: ["x"]),
            ]),
            ("Meeting", [
                Entry(title: "What did we decide", keys: ["d"]),
                Entry(title: "Send the follow-up", keys: ["cmd+return"]),
            ]),
        ],
        [
            ("Change", [
                Entry(title: "Status", keys: ["s"]),
                Entry(title: "Priority", keys: ["p"]),
                Entry(title: "Assign", keys: ["a"]),
                Entry(title: "Assign to me", keys: ["i"]),
                Entry(title: "Due date", keys: ["d"]),
                Entry(title: "Reschedule overdue", keys: ["shift+d"]),
                Entry(title: "Labels", keys: ["l"]),
                Entry(title: "Project", keys: ["shift+p"]),
                Entry(title: "Rename", keys: ["e"]),
                Entry(title: "Snooze", keys: ["h"]),
                Entry(title: "Move to cycle", keys: ["shift+c"]),
                Entry(title: "Book time", keys: ["b"]),
                Entry(title: "Nudge", keys: ["r"]),
                Entry(title: "Archive", keys: ["cmd+backspace"]),
                Entry(title: "Undo", keys: ["cmd+z"]),
            ]),
            ("Agents", [
                Entry(title: "Delegate", keys: ["shift+cmd+;"]),
                Entry(title: "Accept change", keys: ["y"]),
                Entry(title: "Accept all changes", keys: ["shift+y"]),
                Entry(title: "Reject change", keys: ["n"]),
            ]),
        ],
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Keyboard shortcuts")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Theme.ink)
                Spacer(minLength: 0)
                Button("Done") { model.workFocus.shortcutsShown = false }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
            }
            .padding(.horizontal, 20)
            .frame(height: 52)
            Hairline()
            HStack(alignment: .top, spacing: 32) {
                ForEach(Array(Self.columns.enumerated()), id: \.offset) { _, column in
                    VStack(alignment: .leading, spacing: 20) {
                        ForEach(column, id: \.title) { group in
                            VStack(alignment: .leading, spacing: 2) {
                                Text(group.title)
                                    .font(.system(size: 12, weight: .semibold))
                                    .foregroundStyle(Theme.inkTertiary)
                                    .accessibilityAddTraits(.isHeader)
                                    .padding(.bottom, 4)
                                ForEach(group.entries) { entry in
                                    line(entry)
                                }
                            }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                }
            }
            .padding(20)
        }
        .frame(width: 840)
        .background(Theme.elevated)
    }

    private func line(_ entry: Entry) -> some View {
        HStack(spacing: 8) {
            Text(entry.title)
                .font(TypeScale.body(13))
                .foregroundStyle(Theme.inkSecondary)
            Spacer(minLength: 8)
            ForEach(entry.keys, id: \.self) { keys in
                Shortcut(keys)
            }
        }
        .frame(height: 28)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(entry.title), \(entry.keys.joined(separator: " or "))")
    }
}
