import SwiftUI

/// Every dictation, newest first, grouped by day. One search field.
struct LibraryScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if let transcription = model.selectedTranscription {
            TranscriptionDetail(transcription: transcription)
        } else {
            list
        }
    }

    private var list: some View {
        Page {
            PageTitle("Library", subtitle: subtitle) {
                Button {
                } label: {
                    Label("Import audio", systemImage: "waveform.badge.plus")
                }
                .buttonStyle(.primary)
            }
            HStack(spacing: 12) {
                SearchField(prompt: "Search transcripts", text: Bindable(model).historyQuery)
                    .frame(maxWidth: 380)
                Spacer()
                Button {
                } label: {
                    Label("Open recordings folder", systemImage: "folder")
                }
                .buttonStyle(.secondary)
            }
            .padding(.bottom, 28)
            ForEach(days, id: \.day) { group in
                PageSection(group.day) {
                    Card {
                        ForEach(group.items) { transcription in
                            CardRow {
                                model.selectedTranscription = transcription
                            } leading: {
                                Text(transcription.text)
                                    .bodyText(16)
                                    .lineLimit(2)
                                    .frame(maxWidth: 640, alignment: .leading)
                            } trailing: {
                                HStack(spacing: 24) {
                                    Text("\(transcription.words) words").metaText()
                                    Text(transcription.date.time).metaText()
                                }
                            }
                        }
                    }
                }
            }
            if model.hasMoreTranscriptions {
                Button("Show older") { model.loadMoreTranscriptions() }
                    .buttonStyle(.secondary)
            }
        }
    }

    private var subtitle: String {
        guard let stats = model.stats else { return "" }
        return "\(stats.entries) recordings · \((Double(stats.totalDurationMs) / 1000).clock) · \(stats.totalWords) words"
    }

    private var filtered: [Transcription] {
        let query = model.historyQuery.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else { return model.transcriptions }
        return model.transcriptions.filter { $0.text.localizedCaseInsensitiveContains(query) }
    }

    private var days: [(day: String, items: [Transcription])] {
        var order: [String] = []
        var groups: [String: [Transcription]] = [:]
        for transcription in filtered {
            let day = transcription.date.relativeDay
            if groups[day] == nil { order.append(day) }
            groups[day, default: []].append(transcription)
        }
        return order.map { ($0, groups[$0] ?? []) }
    }
}

struct TranscriptionDetail: View {
    @Environment(AppModel.self) private var model
    let transcription: Transcription

    var body: some View {
        Page {
            BackLink(title: "Library") { model.selectedTranscription = nil }
            PageTitle("\(transcription.date.relativeDay), \(transcription.date.time)",
                      subtitle: transcription.rawText == transcription.text ? "\(transcription.words) words" : "\(transcription.words) words · polished") {
                Button("Copy") { model.copy(transcription) }.buttonStyle(.primary)
            }
            Card {
                Text(transcription.text)
                    .bodyText(17)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(24)
            }
            .padding(.bottom, 32)
            if transcription.rawText != transcription.text {
                PageSection("As heard") {
                    Card {
                        Text(transcription.rawText)
                            .bodyText(16, Theme.inkSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                            .padding(24)
                    }
                }
                .padding(.bottom, 32)
            }
            Card {
                CardRow {
                    Text("Delete this recording").bodyText(15, Theme.inkSecondary)
                } trailing: {
                    Button("Delete") { model.delete(transcription) }.buttonStyle(.quiet)
                }
            }
        }
    }
}
