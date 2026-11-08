import SwiftUI

/// ⇧V: how the list in front lays out, groups and orders its tasks, and
/// which properties its rows show. My work orders itself, so there it is
/// only the properties.
struct DisplayOptionsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        let draft = focus.listing(model.work.views)
        let shows = Set(draft?.display.properties ?? focus.myProperties)
        VStack(alignment: .leading, spacing: 12) {
            if let display = draft?.display {
                line("Layout") {
                    SegmentedSwitch(
                        items: [
                            SwitchItem(id: WorkLayout.list, title: "List", icon: "list.bullet"),
                            SwitchItem(id: WorkLayout.board, title: "Board", icon: "square.split.3x1"),
                        ],
                        selection: display.layout
                    ) { layout in
                        model.editListing { $0.display.layout = layout }
                    }
                }
                line("Grouping") {
                    Picker("Grouping", selection: Binding(
                        get: { display.groupBy },
                        set: { group in model.editListing { $0.display.groupBy = group } }
                    )) {
                        ForEach(WorkGroupBy.allCases, id: \.self) { Text(Self.title($0)).tag($0) }
                    }
                    .pickerStyle(.menu)
                    .labelsHidden()
                    .fixedSize()
                }
                line("Ordering") {
                    Picker("Ordering", selection: Binding(
                        get: { display.orderBy },
                        set: { order in model.editListing { $0.display.orderBy = order } }
                    )) {
                        ForEach(WorkOrderBy.allCases.filter { $0 != .manual || display.orderBy == .manual }, id: \.self) {
                            Text(Self.title($0)).tag($0)
                        }
                    }
                    .pickerStyle(.menu)
                    .labelsHidden()
                    .fixedSize()
                }
                line("Empty groups") {
                    Toggle("Empty groups", isOn: Binding(
                        get: { display.showEmptyGroups },
                        set: { on in model.editListing { $0.display.showEmptyGroups = on } }
                    ))
                    .toggleStyle(.switch)
                    .controlSize(.mini)
                    .labelsHidden()
                }
                Hairline().padding(.vertical, 2)
            }
            Text("Properties")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.inkTertiary)
                .accessibilityAddTraits(.isHeader)
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 86), spacing: 6)], alignment: .leading, spacing: 6) {
                ForEach(TaskProperty.allCases) { property in
                    let on = shows.contains(property.rawValue)
                    Button {
                        toggle(property, in: shows)
                    } label: {
                        Text(property.label)
                            .font(TypeScale.label(12))
                            .foregroundStyle(on ? Theme.ink : Theme.inkSecondary)
                            .frame(maxWidth: .infinity)
                            .frame(height: 26)
                            .background(on ? Theme.selection : Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(on ? .isSelected : [])
                }
            }
        }
        .padding(16)
        .frame(width: 320)
    }

    private func line<Control: View>(_ title: String, @ViewBuilder control: () -> Control) -> some View {
        HStack(spacing: 12) {
            Text(title).bodyText(13, Theme.inkSecondary)
            Spacer(minLength: 8)
            control()
        }
        .frame(minHeight: 28)
    }

    /// Properties keep one order however they are switched on.
    private func toggle(_ property: TaskProperty, in shows: Set<String>) {
        var next = shows
        if next.contains(property.rawValue) { next.remove(property.rawValue) } else { next.insert(property.rawValue) }
        let ordered = TaskProperty.allCases.map(\.rawValue).filter(next.contains)
        if model.workFocus.listing(model.work.views) == nil {
            model.workFocus.myProperties = ordered
        } else {
            model.editListing { $0.display.properties = ordered }
        }
    }

    static func title(_ group: WorkGroupBy) -> String {
        switch group {
        case .status: "Status"
        case .owner: "Owner"
        case .project: "Project"
        case .meeting: "Meeting"
        case .due: "Due date"
        case .priority: "Priority"
        case .none: "No grouping"
        }
    }

    static func title(_ order: WorkOrderBy) -> String {
        switch order {
        case .priority: "Priority"
        case .due: "Due date"
        case .updated: "Last updated"
        case .created: "Created"
        case .manual: "Manual"
        }
    }
}
