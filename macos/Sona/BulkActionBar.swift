import SwiftUI

/// Over the list while tasks are selected: how many, and every verb that
/// fits them all. Esc clears the selection.
struct BulkActionBar: View {
    let place: Place
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        let ids = model.workTargets
        let rows = model.workRows(ids)
        HStack(spacing: 4) {
            Text(ids.count == 1 ? "1 selected" : "\(ids.count) selected")
                .font(TypeScale.listTitle)
                .monospacedDigit()
                .foregroundStyle(Theme.ink)
                .padding(.leading, 10)
            WorkBarButton(icon: "xmark", help: "Clear selection  Esc") { focus.clearSelection() }
            Hairline(axis: .vertical).frame(height: 18).padding(.horizontal, 4)
            if place == .triage {
                TriageButtons(ids: ids)
            } else {
                ForEach([WorkAction.status, .priority, .owner, .due, .labels, .project].filter { $0.applies(to: rows) }) { action in
                    if let kind = action.picker {
                        WorkPickerButton(kind: kind, ids: ids) {
                            BulkLabel(action: action)
                        }
                        .help("\(action.title)  \(action.keys.uppercased())")
                    }
                }
                if WorkAction.archive.applies(to: rows) {
                    Button {
                        model.perform(.archive, on: ids)
                    } label: {
                        BulkLabel(action: .archive)
                    }
                    .buttonStyle(.plain)
                    .help("Archive  ⌘⌫")
                }
            }
        }
        .padding(6)
        .background {
            RoundedRectangle(cornerRadius: Theme.radiusCard)
                .fill(Theme.elevated)
                .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.border, lineWidth: 1))
                .elevation(.medium)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Selected tasks")
    }
}

private struct BulkLabel: View {
    let action: WorkAction
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: action.icon).font(.system(size: 11, weight: .medium))
            Text(action.title)
        }
        .font(TypeScale.label(12))
        .foregroundStyle(Theme.ink)
        .padding(.horizontal, 8)
        .frame(height: 28)
        .background(hovering ? Theme.controlHover : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .hoverFade(hovering)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}
