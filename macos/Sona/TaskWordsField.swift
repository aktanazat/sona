import SwiftUI

/// Typed words read as filter conditions, the reading `work list --where` makes.
struct WorkWhere: Decodable {
    let terms: [WorkWhereTerm]
    let unread: [WorkWhereWord]
}

struct WorkWhereTerm: Decodable {
    let text: String
    let label: String
    let condition: WorkCondition
}

/// Words that became no condition; `couldBe` names each thing they could mean.
struct WorkWhereWord: Decodable, Hashable {
    let text: String
    let couldBe: [String]
}

private struct WorkWhereParams: Encodable { let text: String }

/// Words typed into the filter bar join its chips as conditions; the words
/// it could not read go to `unread`, shown among the chips until dismissed.
struct TaskWordsField: View {
    @Binding var unread: [WorkWhereWord]
    @Environment(AppModel.self) private var model
    @State private var words = ""
    @State private var failure: String?

    var body: some View {
        HStack(spacing: 6) {
            SearchField(prompt: "Filter in words", text: $words)
                .frame(width: 200)
                .onSubmit(read)
                .accessibilityLabel("Filter in words")
            if let failure {
                Text(failure).metaText(Theme.live).lineLimit(1)
            }
        }
        .onChange(of: model.workFocus.tab) {
            unread = []
            failure = nil
        }
    }

    private func read() {
        let text = words.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        Task {
            do {
                let read: WorkWhere = try await model.core.request("work_where", WorkWhereParams(text: text))
                let conditions = read.terms.map(\.condition)
                if !conditions.isEmpty {
                    model.editListing { listing in
                        let known = listing.query.filters ?? []
                        listing.query.filters = known + conditions.filter { !known.contains($0) }
                    }
                }
                unread = read.unread
                failure = nil
                words = ""
            } catch {
                failure = error.localizedDescription
            }
        }
    }
}

struct TaskUnreadWordChip: View {
    let word: WorkWhereWord
    let dismiss: () -> Void

    var body: some View {
        let text = word.couldBe.isEmpty
            ? "Not understood: \(word.text)"
            : "\(word.text) could be \(word.couldBe.joined(separator: " or "))"
        HStack(spacing: 6) {
            Text(text).lineLimit(1)
            Button(action: dismiss) {
                Image(systemName: "xmark")
                    .font(.system(size: 8, weight: .bold))
                    .frame(width: 14, height: 14)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Dismiss \(text)")
        }
        .font(TypeScale.label(12))
        .foregroundStyle(Theme.inkTertiary)
        .padding(.leading, 8)
        .padding(.trailing, 4)
        .frame(height: Theme.compactHeight)
        .overlay {
            RoundedRectangle(cornerRadius: Theme.radiusControl)
                .strokeBorder(Theme.border, style: StrokeStyle(lineWidth: 1, dash: [3, 2]))
        }
    }
}
