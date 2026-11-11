import SwiftUI

struct SuggestionActions: View {
    let itemId: String
    let suggestion: WorkFieldSuggestion
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(spacing: 6) {
            Button("Dismiss") {
                Task { await model.work.dismissSuggestion(suggestion, on: itemId) }
            }
            .buttonStyle(.quiet)
            .help("Stop suggesting this for the task")
            Button("Use") {
                Task { await model.work.acceptSuggestion(suggestion, on: itemId) }
            }
            .buttonStyle(QuietButton(compact: true))
        }
    }
}

struct SuggestionCard: View {
    let itemId: String
    let suggestion: WorkFieldSuggestion
    let now: Int64
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: WorkSuggestionText.icon(suggestion.field))
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Theme.accent)
                Text(WorkSuggestionText.title(suggestion.field)).metaText()
                Text(WorkSuggestionText.value(suggestion, model: model, now: now))
                    .font(TypeScale.listTitle)
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
            }
            Text(suggestion.reason)
                .bodyText(12, Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
            if let quote = suggestion.quote {
                Text("“\(quote)”")
                    .bodyText(12, Theme.inkSecondary)
                    .italic()
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Spacer(minLength: 0)
                SuggestionActions(itemId: itemId, suggestion: suggestion)
            }
        }
        .padding(14)
        .frame(width: 300, alignment: .leading)
    }
}
