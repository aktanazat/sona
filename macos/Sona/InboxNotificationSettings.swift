import SwiftUI

/// Settings > Tasks: which kinds of notice reach the inbox and which show a
/// banner, and the hours banners keep quiet.
struct InboxNotificationSettings: View {
    @Environment(AppModel.self) private var model
    @State private var settings: WorkInboxSettings?
    @State private var loadFailed = false
    @State private var error: String?

    var body: some View {
        PageSection("Inbox notifications") {
            ErrorNote(error ?? (loadFailed ? "Couldn't load the inbox settings." : nil))
            Card {
                if let settings {
                    HStack(spacing: 16) {
                        Spacer()
                        Text("Inbox").frame(width: Self.column)
                        Text("Banner").frame(width: Self.column)
                    }
                    .metaText(Theme.inkSecondary)
                    .padding(.horizontal, 16)
                    .padding(.top, 12)
                    .accessibilityHidden(true)
                    ForEach(settings.kinds.filter { $0.kind != .unknown }, id: \.kind) { setting in
                        row(setting)
                    }
                } else if !loadFailed {
                    CardLine("Reading the inbox settings…")
                }
            }
            if let settings {
                Card {
                    ToggleRow(
                        title: "Dock badge",
                        detail: "Show the unread count on the Dock icon.",
                        isOn: Binding(get: { settings.dockBadge }, set: { setDockBadge($0) }))
                    ToggleRow(
                        title: "Quiet hours",
                        detail: "Banners wait until they end. Notices still reach the inbox.",
                        isOn: Binding(get: { settings.quietHours != nil },
                                      set: { on in setQuiet(on ? WorkQuietHours(startMinute: 22 * 60, endMinute: 8 * 60) : nil) }))
                    if let quiet = settings.quietHours {
                        CardRow {
                            Text("From").bodyText()
                        } trailing: {
                            HStack(spacing: 8) {
                                DatePicker("From", selection: Binding(
                                    get: { Self.date(quiet.startMinute) },
                                    set: { setQuiet(WorkQuietHours(startMinute: Self.minute($0), endMinute: quiet.endMinute)) }),
                                    displayedComponents: .hourAndMinute)
                                    .labelsHidden()
                                Text("to").metaText(Theme.inkSecondary)
                                DatePicker("To", selection: Binding(
                                    get: { Self.date(quiet.endMinute) },
                                    set: { setQuiet(WorkQuietHours(startMinute: quiet.startMinute, endMinute: Self.minute($0))) }),
                                    displayedComponents: .hourAndMinute)
                                    .labelsHidden()
                            }
                        }
                    }
                }
            }
        }
        .task { await load() }
    }

    private func row(_ setting: WorkInboxKindSetting) -> some View {
        CardRow {
            Text(setting.kind.label).bodyText()
        } trailing: {
            HStack(spacing: 16) {
                Toggle("In inbox", isOn: Binding(get: { setting.inbox }, set: { set(setting.kind, inbox: $0) }))
                    .labelsHidden()
                    .frame(width: Self.column)
                Toggle("Banner", isOn: Binding(get: { setting.inbox && setting.banner },
                                               set: { set(setting.kind, banner: $0) }))
                    .labelsHidden()
                    .frame(width: Self.column)
                    .disabled(!setting.inbox)
            }
            .toggleStyle(.checkbox)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(setting.kind.label)
    }

    private static let column: CGFloat = 48

    private func load() async {
        do {
            settings = try await model.work.inboxSettings()
            loadFailed = false
        } catch {
            loadFailed = true
        }
    }

    private func set(_ kind: WorkNotificationKind, inbox: Bool? = nil, banner: Bool? = nil) {
        guard var next = settings else { return }
        for index in next.kinds.indices where next.kinds[index].kind == kind {
            if let inbox { next.kinds[index].inbox = inbox }
            if let banner { next.kinds[index].banner = banner }
        }
        let kept = next.kinds.filter { $0.kind != .unknown }
        let change = inbox != nil
            ? WorkInboxSettingsChange(inbox: kept.filter(\.inbox).map(\.kind))
            : WorkInboxSettingsChange(banners: kept.filter(\.banner).map(\.kind))
        save(change, showing: next)
    }

    private func setQuiet(_ hours: WorkQuietHours?) {
        guard var next = settings, next.quietHours != hours else { return }
        next.quietHours = hours
        save(WorkInboxSettingsChange(quietHours: .some(hours)), showing: next)
    }

    private func setDockBadge(_ on: Bool) {
        guard var next = settings, next.dockBadge != on else { return }
        next.dockBadge = on
        save(WorkInboxSettingsChange(dockBadge: on), showing: next)
    }

    private func save(_ change: WorkInboxSettingsChange, showing next: WorkInboxSettings) {
        guard let before = settings else { return }
        settings = next
        Task {
            do {
                settings = try await model.work.saveInboxSettings(change)
                error = nil
            } catch {
                settings = before
                self.error = "Couldn't save the inbox settings."
            }
        }
    }

    private static func date(_ minute: Int) -> Date {
        let calendar = Calendar.current
        return calendar.date(byAdding: .minute, value: minute, to: calendar.startOfDay(for: .now)) ?? .now
    }

    private static func minute(_ date: Date) -> Int {
        let parts = Calendar.current.dateComponents([.hour, .minute], from: date)
        return (parts.hour ?? 0) * 60 + (parts.minute ?? 0)
    }
}
