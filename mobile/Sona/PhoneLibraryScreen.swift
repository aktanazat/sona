import SwiftUI
import UniformTypeIdentifiers
import UIKit

struct PhoneLibraryScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject var library: PhoneLibrary
    @State private var filter: PhoneNote.Kind?
    @State private var newNote = false
    @State private var draft = ""
    @State private var importing = false
    @State private var importBusy = false
    @State private var search = ""

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Picker("library.filter", selection: $filter) {
                        Text("library.all").tag(PhoneNote.Kind?.none)
                        Text("library.notes").tag(PhoneNote.Kind?.some(.note))
                        Text("library.dictations").tag(PhoneNote.Kind?.some(.dictation))
                        Text("library.meetings").tag(PhoneNote.Kind?.some(.meeting))
                    }
                    .pickerStyle(.segmented)
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets())
                    .listRowSeparator(.hidden)
                    if let error = library.error {
                        Text(error)
                            .font(.footnote)
                            .foregroundStyle(Theme.recording)
                            .listRowBackground(Color.clear)
                            .listRowInsets(EdgeInsets(top: 8, leading: 4, bottom: 0, trailing: 4))
                            .listRowSeparator(.hidden)
                    }
                }
                if importBusy {
                    Section {
                        ProgressView("library.importing")
                    }
                    .listRowBackground(Theme.surface)
                }
                if visible.isEmpty && remoteMeetings.isEmpty {
                    Section {
                        VStack(spacing: 4) {
                            Text("library.empty")
                                .font(.body.weight(.semibold))
                                .foregroundStyle(Theme.textPrimary)
                            Text("library.emptyHint")
                                .font(.subheadline)
                                .foregroundStyle(Theme.textSecondary)
                        }
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: .infinity, minHeight: 120)
                        .listRowBackground(Color.clear)
                    }
                }
                ForEach(days) { day in
                    Section {
                        row(day.first)
                    } header: {
                        Text(Self.dayTitle.string(from: day.first.date))
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Theme.textSecondary)
                            .textCase(nil)
                    }
                    ForEach(day.rest) { note in
                        Section { row(note) }
                    }
                }
                if filter == nil || filter == .meeting {
                    Section {
                        if model.boardState == .syncing { ProgressView("board.syncing") }
                        if model.boardState == .offline {
                            Text("board.offline").font(.footnote).foregroundStyle(Theme.textSecondary)
                        }
                        if !model.isPaired {
                            Text("library.pairToRead").font(.footnote).foregroundStyle(Theme.textSecondary)
                        } else if remoteMeetings.isEmpty && model.boardState != .syncing {
                            Text("library.noMacMeetings").font(.footnote).foregroundStyle(Theme.textSecondary)
                        }
                        ForEach(remoteMeetings) { meeting in
                            NavigationLink {
                                PhoneMeetingScreen(meeting: meeting)
                            } label: {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(meeting.session.title)
                                        .font(.body.weight(.semibold))
                                        .foregroundStyle(Theme.textPrimary)
                                    Text(meeting.date, style: .date)
                                        .font(.footnote)
                                        .foregroundStyle(Theme.textTertiary)
                                }
                                .padding(.vertical, 4)
                            }
                        }
                    } header: {
                        Text("library.fromMac")
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Theme.textSecondary)
                            .textCase(nil)
                    }
                    .listRowBackground(Theme.surface)
                }
                Section {
                    NavigationLink("calls.title") { PhoneCallsScreen(model: model, calls: model.calls) }
                        .foregroundStyle(Theme.textPrimary)
                    Button("library.importCall") { importing = true }
                        .disabled(importBusy || model.recorder.isRecording)
                } header: {
                    Text("library.calls")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.textSecondary)
                        .textCase(nil)
                } footer: {
                    Text("library.callExplanation")
                        .foregroundStyle(Theme.textSecondary)
                }
                .listRowBackground(Theme.surface)
            }
            .listStyle(.insetGrouped)
            .listSectionSpacing(.custom(12))
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .navigationTitle("library.title")
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $search)
            .task { await model.refreshBoard() }
            .refreshable { await model.refreshBoard() }
            .toolbar {
                Button { newNote = true } label: { Label("library.new", systemImage: "square.and.pencil") }
            }
            .sheet(isPresented: $newNote) {
                NavigationStack {
                    TextEditor(text: $draft)
                        .font(.body)
                        .foregroundStyle(Theme.textPrimary)
                        .scrollContentBackground(.hidden)
                        .overlay(alignment: .topLeading) {
                            if draft.isEmpty {
                                Text("library.noteText")
                                    .foregroundStyle(Theme.textTertiary)
                                    .padding(.top, 8)
                                    .padding(.leading, 5)
                                    .allowsHitTesting(false)
                                    .accessibilityHidden(true)
                            }
                        }
                        .padding(.horizontal, Theme.pageInset)
                        .background(Theme.background)
                        .accessibilityLabel("library.noteText")
                        .navigationTitle("library.new")
                        .navigationBarTitleDisplayMode(.inline)
                        .toolbar {
                            ToolbarItem(placement: .cancellationAction) { Button("dictation.cancel") { newNote = false } }
                            ToolbarItem(placement: .confirmationAction) {
                                Button("capture.save") {
                                    if model.captureTyped(draft, images: []) { draft = ""; newNote = false }
                                }.disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                            }
                        }
                }
                .tint(Theme.accent)
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.audio]) { result in
                switch result {
                case .failure(let error): library.error = error.localizedDescription
                case .success(let url):
                    importBusy = true
                    Task { await model.importRecording(url); importBusy = false }
                }
            }
        }
        .tint(Theme.accent)
    }

    private func row(_ note: PhoneNote) -> some View {
        NavigationLink {
            PhoneNoteScreen(model: model, library: library, note: note)
        } label: {
            VStack(alignment: .leading, spacing: 6) {
                if note.text.isEmpty || !note.text.hasPrefix(note.title) {
                    Text(note.title.isEmpty ? NSLocalizedString("library.untitled", comment: "") : note.title)
                        .font(.body.weight(.semibold))
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(2)
                }
                if !note.text.isEmpty {
                    Text(note.text)
                        .font(.body)
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(3)
                }
                HStack(spacing: 6) {
                    Image(systemName: Self.symbol(note.kind))
                        .accessibilityHidden(true)
                    Text(note.date, style: .time)
                    if let duration = note.duration {
                        Text(Duration.seconds(duration).formatted(.time(pattern: .minuteSecond)))
                    }
                }
                .font(.footnote)
                .foregroundStyle(Theme.textTertiary)
            }
            .padding(.vertical, 6)
        }
        .navigationLinkIndicatorVisibility(.hidden)
        .swipeActions {
            Button("library.delete", role: .destructive) {
                do { try library.delete(note) } catch { library.error = error.localizedDescription }
            }
        }
        .listRowBackground(Theme.surface)
    }

    private static func symbol(_ kind: PhoneNote.Kind) -> String {
        switch kind {
        case .note: return "text.alignleft"
        case .dictation: return "mic"
        case .meeting: return "waveform"
        }
    }

    private static let dayTitle: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .none
        formatter.doesRelativeDateFormatting = true
        return formatter
    }()

    private struct DayRun: Identifiable {
        let first: PhoneNote
        var rest: [PhoneNote] = []
        var id: UUID { first.id }
    }

    /// Runs of notes from the same day, in library order. A day that recurs later in the
    /// list starts a new run rather than pulling its notes out of order.
    private var days: [DayRun] {
        let calendar = Calendar.current
        var runs: [DayRun] = []
        for note in visible {
            if let last = runs.last, calendar.isDate(last.first.date, inSameDayAs: note.date) {
                runs[runs.count - 1].rest.append(note)
            } else {
                runs.append(DayRun(first: note))
            }
        }
        return runs
    }

    private var visible: [PhoneNote] {
        library.notes.filter {
            (filter == nil || $0.kind == filter) &&
                (search.isEmpty || $0.title.localizedStandardContains(search) || $0.text.localizedStandardContains(search))
        }
    }
    private var remoteMeetings: [PhoneMeeting] {
        guard filter == nil || filter == .meeting else { return [] }
        return (model.board?.meetings ?? []).filter {
            search.isEmpty || $0.session.title.localizedStandardContains(search) ||
                $0.notes.localizedStandardContains(search) || $0.transcript.localizedStandardContains(search)
        }
    }
}

private struct PhoneNoteScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject var library: PhoneLibrary
    @State var note: PhoneNote
    @StateObject private var playback = NotePlayback()
    @State private var saved = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("library.noteTitle", text: $note.title)
                .font(.title3.weight(.semibold))
                .foregroundStyle(Theme.textPrimary)
                .frame(minHeight: 44)
            HStack(spacing: 6) {
                Text(note.date.formatted(date: .abbreviated, time: .shortened))
                if saved {
                    Text("library.saved")
                }
            }
            .font(.footnote)
            .foregroundStyle(Theme.textTertiary)
            TextEditor(text: $note.text)
                .font(.body)
                .foregroundStyle(Theme.textPrimary)
                .scrollContentBackground(.hidden)
                .frame(maxHeight: .infinity)
                .padding(.horizontal, -5)
                .accessibilityLabel("library.noteText")
            if let error = library.error {
                Text(error)
                    .font(.footnote)
                    .foregroundStyle(Theme.recording)
            }
            if library.audioURL(note) != nil {
                audio
            }
        }
        .padding(.horizontal, Theme.pageInset)
        .padding(.bottom, 8)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.background)
        .navigationTitle("library.note")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .toolbar {
            ToolbarItem(placement: .bottomBar) {
                Button("library.saveChanges") {
                    do { try library.update(note); saved = true } catch { library.error = error.localizedDescription }
                }
                .disabled(saved)
            }
            ToolbarSpacer(.flexible, placement: .bottomBar)
            ToolbarItemGroup(placement: .bottomBar) {
                Button { UIPasteboard.general.string = note.text } label: {
                    Label("library.copy", systemImage: "doc.on.doc")
                }
                ShareLink(item: note.text) { Label("library.share", systemImage: "square.and.arrow.up") }
            }
        }
        .onChange(of: note) { _, _ in saved = false }
        .task { if let url = library.audioURL(note) { playback.load(url) } }
        .onDisappear { playback.stop() }
    }

    private var audio: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 12) {
                Button {
                    model.dictation.endWarmSession()
                    playback.toggle()
                } label: {
                    Image(systemName: playback.playing ? "pause.fill" : "play.fill")
                        .font(.body.weight(.semibold))
                        .frame(width: 44, height: 44)
                        .background(Theme.inset, in: Circle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(Theme.textPrimary)
                .disabled(model.recorder.isRecording || model.dictation.isBusy)
                .accessibilityLabel(Text(playback.playing ? "library.pause" : "library.play"))
                Slider(value: Binding(get: { playback.position }, set: { playback.seek($0) }),
                       in: 0...max(playback.duration, 0.001))
                    .accessibilityLabel("library.position")
                Picker("library.speed", selection: $playback.rate) {
                    Text("1×").tag(Float(1)); Text("1.5×").tag(Float(1.5)); Text("2×").tag(Float(2))
                }
                .pickerStyle(.menu)
                .labelsHidden()
                .accessibilityLabel(Text("library.speed"))
            }
            if let error = playback.error {
                Text(error)
                    .font(.footnote)
                    .foregroundStyle(Theme.recording)
            }
        }
        .padding(12)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("library.audio"))
    }
}

private struct PhoneMeetingScreen: View {
    let meeting: PhoneMeeting
    @State private var transcript = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Picker("meeting.content", selection: $transcript) {
                    Text("library.notes").tag(false)
                    Text("meeting.transcript").tag(true)
                }.pickerStyle(.segmented)
                if transcript {
                    Text(meeting.transcript.isEmpty ? NSLocalizedString("meeting.noTranscript", comment: "") : meeting.transcript)
                } else {
                    Text(meeting.notes.isEmpty ? NSLocalizedString("meeting.noNotes", comment: "") : meeting.notes)
                    if let ownNotes = meeting.user_notes?.body, !ownNotes.isEmpty {
                        Text("meeting.ownNotes").font(.headline)
                        Text(ownNotes)
                    }
                }
            }
            .font(.body)
            .foregroundStyle(Theme.textPrimary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Theme.pageInset)
            .textSelection(.enabled)
        }
        .background(Theme.background)
        .navigationTitle(meeting.session.title).navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ShareLink(item: [meeting.session.title, meeting.notes, meeting.user_notes?.body ?? "", meeting.transcript]
                .filter { !$0.isEmpty }.joined(separator: "\n\n")) {
                    Label("library.share", systemImage: "square.and.arrow.up")
                }
        }
    }
}
