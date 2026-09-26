import Combine
import Foundation
import SwiftUI

struct PhoneCallEntry: Codable, Identifiable {
    var id: String
    var accountSID: String
    var target: String
    var date: Date
    var status: String
    var savedRecordings: Set<String> = []
}

@MainActor
final class PhoneCalls: ObservableObject {
    static let enabledKey = "sona.calls.enabled"
    @Published private(set) var calls: [PhoneCallEntry] = []
    @Published private(set) var busy = false
    @Published var error: String?
    private let file: URL

    init() {
        file = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appending(path: "phone-calls.json")
        do { calls = try JSONDecoder().decode([PhoneCallEntry].self, from: Data(contentsOf: file)) }
        catch CocoaError.fileReadNoSuchFile { }
        catch { self.error = error.localizedDescription }
    }

    private func account() throws -> CallAccount {
        guard UserDefaults.standard.bool(forKey: Self.enabledKey), let account = try CallKeychain.load() else { throw CallServiceError.setup }
        return account
    }
    func start(target: String, model: AppModel) async {
        guard !busy else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            let account = try account()
            model.dictation.cancel()
            model.dictation.endWarmSession()
            let target = try CallService.phoneNumber(target)
            let call = try await CallService(account: account).start(target: target)
            calls.insert(PhoneCallEntry(id: call.sid, accountSID: account.accountSID, target: target, date: Date(), status: call.status), at: 0)
            try save()
        } catch { self.error = NSLocalizedString("calls.startFailed", comment: "") + " " + error.localizedDescription }
    }
    func refresh() async {
        guard !busy, UserDefaults.standard.bool(forKey: Self.enabledKey) else { return }
        busy = true
        defer { busy = false }
        do {
            let account = try account()
            let service = try CallService(account: account)
            for index in calls.indices where calls[index].accountSID == account.accountSID && !["completed", "failed", "busy", "no-answer", "canceled"].contains(calls[index].status) {
                calls[index].status = try await service.call(calls[index].id).status
            }
            try save()
            error = nil
        } catch { self.error = error.localizedDescription }
    }
    func end(_ call: PhoneCallEntry) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        do {
            let account = try account()
            guard account.accountSID == call.accountSID else { throw CallServiceError.setup }
            try await CallService(account: account).end(call.id)
            if let index = calls.firstIndex(where: { $0.id == call.id }) { calls[index].status = "completed" }
            try save()
            error = nil
        } catch { self.error = error.localizedDescription }
    }
    func keep(_ call: PhoneCallEntry, model: AppModel) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        do {
            let account = try account()
            guard account.accountSID == call.accountSID else { throw CallServiceError.setup }
            let service = try CallService(account: account)
            let recordings = try await service.recordings(call.id).filter { $0.status == "completed" }
            guard !recordings.isEmpty else { error = NSLocalizedString("calls.recordingPending", comment: ""); return }
            for recording in recordings where !call.savedRecordings.contains(recording.id) {
                let file = try await service.download(recording.id)
                let kept = await model.importRecording(file, title: "\(NSLocalizedString("title.call", comment: "")) \(call.target)", recordedAt: call.date)
                try? FileManager.default.removeItem(at: file)
                guard kept else { error = model.library.error; return }
                if let index = calls.firstIndex(where: { $0.id == call.id }) { calls[index].savedRecordings.insert(recording.id) }
                try save()
            }
            error = nil
        } catch { self.error = error.localizedDescription }
    }
    private func save() throws {
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(calls).write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }
}

struct PhoneCallsScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject var calls: PhoneCalls
    @AppStorage(PhoneCalls.enabledKey) private var enabled = false
    @Environment(\.scenePhase) private var scenePhase
    @State private var setup = false
    @State private var target = ""
    @State private var consent = false
    @State private var confirm = false

    var body: some View {
        Form {
            Section {
                Toggle("calls.enable", isOn: $enabled)
                Text("calls.privacy").font(.footnote)
                Button("calls.settings") { setup = true }
            }
            if enabled {
                Section("calls.outgoing") {
                    Text("calls.instructions").font(.footnote)
                    TextField("calls.recipient", text: $target).keyboardType(.phonePad)
                    Toggle("calls.consent", isOn: $consent)
                    Button("calls.start") { confirm = true }
                        .disabled(!consent || calls.busy || target.isEmpty || model.recorder.isRecording || !model.consentAccepted)
                    if calls.busy { ProgressView() }
                    if let error = calls.error { Text(error).foregroundStyle(Theme.recording) }
                }
                Section("calls.history") {
                    if calls.calls.isEmpty { Text("calls.empty").foregroundStyle(Theme.textSecondary) }
                    ForEach(calls.calls) { call in
                        VStack(alignment: .leading, spacing: 8) {
                            Text(call.target).font(.headline)
                            Text(call.date, style: .date).font(.caption)
                            Text(LocalizedStringKey("calls.status.\(call.status)")).font(.footnote)
                            if ["queued", "ringing", "in-progress"].contains(call.status) {
                                Button("calls.end", role: .destructive) { Task { await calls.end(call) } }.disabled(calls.busy)
                            } else if call.status == "completed" {
                                Button(call.savedRecordings.isEmpty ? "calls.keep" : "calls.kept") {
                                    Task { await calls.keep(call, model: model) }
                                }.disabled(calls.busy || !call.savedRecordings.isEmpty)
                            }
                        }.padding(.vertical, 4)
                    }
                    Button("calls.refresh") { Task { await calls.refresh() } }.disabled(calls.busy)
                }
            }
        }
        .navigationTitle("calls.title")
        .sheet(isPresented: $setup) { CallSetupScreen() }
        .confirmationDialog("calls.confirm", isPresented: $confirm, titleVisibility: .visible) {
            Button("calls.start") { consent = false; Task { await calls.start(target: target, model: model) } }
        } message: { Text("calls.confirmBody \(target)") }
        .task { await calls.refresh() }
        .onChange(of: scenePhase) { _, phase in if phase == .active { Task { await calls.refresh() } } }
    }
}

private struct CallSetupScreen: View {
    @Environment(\.dismiss) private var dismiss
    @State private var accountSID = ""
    @State private var token = ""
    @State private var from = ""
    @State private var own = ""
    @State private var configured = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Text("calls.setupHelp").font(.footnote)
                TextField("calls.account", text: $accountSID).textInputAutocapitalization(.never).autocorrectionDisabled()
                SecureField(configured ? "calls.keepToken" : "calls.token", text: $token).textInputAutocapitalization(.never)
                TextField("calls.from", text: $from).keyboardType(.phonePad)
                TextField("calls.own", text: $own).keyboardType(.phonePad)
                Text("calls.retention").font(.footnote)
                if let error { Text(error).foregroundStyle(Theme.recording) }
                Button("calls.save") {
                    do {
                        let prior = try CallKeychain.load()
                        let account = CallAccount(accountSID: accountSID.trimmingCharacters(in: .whitespacesAndNewlines),
                            authToken: token.isEmpty ? (prior?.authToken ?? "") : token.trimmingCharacters(in: .whitespacesAndNewlines),
                            fromNumber: try CallService.phoneNumber(from), ownNumber: try CallService.phoneNumber(own))
                        _ = try CallService(account: account)
                        try CallKeychain.save(account)
                        dismiss()
                    } catch { self.error = error.localizedDescription }
                }
                if configured {
                    Button("calls.disconnect", role: .destructive) {
                        do { try CallKeychain.remove(); UserDefaults.standard.set(false, forKey: PhoneCalls.enabledKey); dismiss() }
                        catch { self.error = error.localizedDescription }
                    }
                }
            }
            .navigationTitle("calls.settings")
            .toolbar { Button("library.done") { dismiss() } }
            .task {
                do {
                    if let account = try CallKeychain.load() {
                        configured = true; accountSID = account.accountSID; from = account.fromNumber; own = account.ownNumber
                    }
                } catch { self.error = error.localizedDescription }
            }
        }
    }
}
