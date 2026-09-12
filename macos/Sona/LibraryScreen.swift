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
            PageTitle("Library", subtitle: "\(SampleData.transcriptions.count) recordings · \(totalDuration.clock) · \(words) words") {
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
                                    Text("\(transcription.text.split(separator: " ").count) words").metaText()
                                    Text(transcription.date.time).metaText()
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    private var filtered: [Transcription] {
        let query = model.historyQuery.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else { return SampleData.transcriptions }
        return SampleData.transcriptions.filter { $0.text.localizedCaseInsensitiveContains(query) }
    }

    private var totalDuration: TimeInterval {
        SampleData.transcriptions.reduce(0) { $0 + $1.duration }
    }

    private var words: Int {
        SampleData.transcriptions.reduce(0) { $0 + $1.text.split(separator: " ").count }
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
                      subtitle: "\(transcription.duration.clock) · \(transcription.mode) mode · pasted into \(transcription.app)") {
                HStack(spacing: 10) {
                    Button("Copy") {}.buttonStyle(.primary)
                    Button("Paste again") {}.buttonStyle(.secondary)
                }
            }
            Card {
                Text(transcription.text)
                    .bodyText(17)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(24)
            }
            .padding(.bottom, 32)
            PageSection("Also kept") {
                Card {
                    CardRow {
                        Text("The words as heard, before \(transcription.mode) mode shaped them.").bodyText()
                    } trailing: {
                        Button("Show") {}.buttonStyle(.compact)
                    }
                    CardRow {
                        Text("Audio, for 24 hours, then removed.").bodyText()
                    } trailing: {
                        Button("Play") {}.buttonStyle(.compact)
                    }
                    CardRow {
                        Text("Delete this recording").bodyText(15, Theme.inkSecondary)
                    } trailing: {
                        Button("Delete") {}.buttonStyle(.quiet)
                    }
                }
            }
        }
    }
}
