import SwiftUI

/// Space: the highlighted task at a glance beside the list, without leaving
/// it. J and K carry the peek along; Space or Esc closes it and Return opens
/// the task's page.
struct TaskPeek: View {
    let row: WorkItemRow
    let place: Place
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        let suggestions = row.status == .triage ? model.work.suggestions[row.id] ?? [] : []
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text("#\(row.number)")
                    .font(TypeScale.mono(12))
                    .foregroundStyle(Theme.inkTertiary)
                Spacer(minLength: 8)
                TaskKeyButton(title: "Open", keys: "return") { model.perform(.open, on: [row.id]) }
                WorkBarButton(icon: "xmark", help: "Close  Space") { focus.peeking = false }
            }
            .padding(.leading, 20)
            .padding(.trailing, 12)
            .frame(height: Theme.headerHeight)
            Hairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    Text(row.title)
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                    if let details = row.details, !details.isEmpty {
                        Text(details)
                            .bodyText(13, Theme.inkSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                    if !suggestions.isEmpty {
                        HStack(spacing: 6) {
                            ForEach(Array(suggestions.enumerated()), id: \.offset) { _, suggestion in
                                SuggestionChip(suggestion: suggestion, now: TaskDates.now())
                            }
                        }
                    }
                    if let quote = row.sourceQuote {
                        QuoteCard(quote: quote, speaker: row.sourceSpeaker, meetingTitle: row.sourceMeetingTitle,
                                  meetingId: row.sourceMeetingId, atMs: row.sourceAtMs)
                    }
                    TaskPropertyList(row: row)
                }
                .padding(20)
            }
        }
        .frame(width: 480)
        .frame(maxHeight: .infinity)
        .background {
            Rectangle()
                .fill(Theme.elevated)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.border).frame(width: 1) }
                .elevation(.high)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Peek at #\(row.number)")
    }
}
