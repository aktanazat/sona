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
                Picker("library.filter", selection: $filter) {
                    Text("library.all").tag(PhoneNote.Kind?.none)
                    Text("library.notes").tag(PhoneNote.Kind?.some(.note))
                    Text("library.dictations").tag(PhoneNote.Kind?.some(.dictation))
                    Text("library.meetings").tag(PhoneNote.Kind?.some(.meeting))
                }
                .pickerStyle(.menu)
                .frame(minHeight: 44)
                .listRowBackground(Theme.background)
                .listRowSeparatorTint(Theme.border)
                if let error = library.error {
                    Text(error)
                        .font(.footnote)
                        .foregroundStyle(Theme.recording)
                        .listRowBackground(Theme.background)
                }
                if importBusy {
                    ProgressView("library.importing")
                        .listRowBackground(Theme.background)
                }
                if visible.isEmpty && remoteMeetings.isEmpty {
                    Text("library.empty")
                        .font(.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                        .frame(minHeight: 44)
                        .listRowBackground(Theme.background)
                        .listRowSeparator(.hidden)
                }
                ForEach(visible) { note in
                    NavigationLink {
                        PhoneNoteScreen(model: model, library: library, note: note)
                    } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(note.title.isEmpty ? NSLocalizedString("library.untitled", comment: "") : note.title)
                                .font(.body.weight(.medium))
                                .foregroundStyle(Theme.textPrimary)
                                .lineLimit(2)
                            Text(note.text)
                                .font(.subheadline)
                                .lineLimit(2)
                                .foregroundStyle(Theme.textSecondary)
                            Text(note.date, style: .date)
                                .font(.caption)
                                .foregroundStyle(Theme.textTertiary)
                        }
                        .padding(.vertical, 4)
                    }
                    .swipeActions {
                        Button("library.delete", role: .destructive) {
                            do { try library.delete(note) } catch { library.error = error.localizedDescription }
                        }
                    }
                }
                .listRowBackground(Theme.background)
                .listRowSeparatorTint(Theme.border)
                if filter == nil || filter == .meeting {
                    Section("library.fromMac") {
                        if model.boardState == .syncing { ProgressView("board.syncing") }
                        if model.boardState == .offline { Text("board.offline").foregroundStyle(Theme.textSecondary) }
                        if !model.isPaired { Text("library.pairToRead").font(.footnote) }
                        else if remoteMeetings.isEmpty && model.boardState != .syncing {
                            Text("library.noMacMeetings").font(.footnote).foregroundStyle(Theme.textSecondary)
                        }
                        ForEach(remoteMeetings) { meeting in
                            NavigationLink {
                                PhoneMeetingScreen(meeting: meeting)
                            } label: {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(meeting.session.title)
                                        .font(.body.weight(.medium))
                                        .foregroundStyle(Theme.textPrimary)
                                    Text(meeting.date, style: .date)
                                        .font(.caption)
                                        .foregroundStyle(Theme.textTertiary)
                                }
                                .padding(.vertical, 4)
                            }
                        }
                    }
                    .textCase(nil)
                    .listRowBackground(Theme.background)
                    .listRowSeparatorTint(Theme.border)
                }
                Section("library.calls") {
                    NavigationLink("calls.title") { PhoneCallsScreen(model: model, calls: model.calls) }
                    Text("library.callExplanation").font(.footnote)
                    Button("library.importCall") { importing = true }
                        .disabled(importBusy || model.recorder.isRecording)
                }
                .textCase(nil)
                .listRowBackground(Theme.background)
                .listRowSeparatorTint(Theme.border)
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .navigationTitle("library.title")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(Theme.background, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
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
                        .padding(Theme.pageInset)
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
        List {
            TextField("library.noteTitle", text: $note.title)
                .font(.body.weight(.medium))
                .foregroundStyle(Theme.textPrimary)
                .frame(minHeight: 44)
                .listRowBackground(Theme.background)
            TextEditor(text: $note.text)
                .font(.body)
                .foregroundStyle(Theme.textPrimary)
                .scrollContentBackground(.hidden)
                .frame(minHeight: 200)
                .accessibilityLabel("library.noteText")
                .listRowBackground(Theme.background)
            Button("library.saveChanges") {
                do { try library.update(note); saved = true } catch { library.error = error.localizedDescription }
            }
            .disabled(saved)
            .listRowBackground(Theme.background)
            if saved {
                Text("library.saved")
                    .foregroundStyle(Theme.textSecondary)
                    .listRowBackground(Theme.background)
            }
            if let error = library.error {
                Text(error)
                    .foregroundStyle(Theme.recording)
                    .listRowBackground(Theme.background)
            }
            if library.audioURL(note) != nil {
                Section("library.audio") {
                    Button(playback.playing ? "library.pause" : "library.play") {
                        model.dictation.endWarmSession()
                        playback.toggle()
                    }.disabled(model.recorder.isRecording || model.dictation.isBusy)
                    Slider(value: Binding(get: { playback.position }, set: { playback.seek($0) }),
                           in: 0...max(playback.duration, 0.001))
                        .accessibilityLabel("library.position")
                    Picker("library.speed", selection: $playback.rate) {
                        Text("1×").tag(Float(1)); Text("1.5×").tag(Float(1.5)); Text("2×").tag(Float(2))
                    }
                    if let error = playback.error { Text(error).foregroundStyle(Theme.recording) }
                }
                .textCase(nil)
                .listRowBackground(Theme.background)
                .listRowSeparatorTint(Theme.border)
            }
            ShareLink(item: note.text) { Label("library.share", systemImage: "square.and.arrow.up") }
                .listRowBackground(Theme.background)
            Button("library.copy") { UIPasteboard.general.string = note.text }
                .listRowBackground(Theme.background)
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .navigationTitle("library.note")
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: note) { _, _ in saved = false }
        .task { if let url = library.audioURL(note) { playback.load(url) } }
        .onDisappear { playback.stop() }
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
