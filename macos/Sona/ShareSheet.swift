import SwiftUI
import WebKit

/// One transcript turn under edit. Each is its own object so a keystroke
/// redraws its row and nothing else, however long the transcript is.
@Observable
final class ShareTurn: Identifiable {
    let id: Int
    let s: Int
    let t: Double
    var text: String

    init(id: Int, turn: SharedTurn) {
        self.id = id
        s = turn.s
        t = turn.t
        text = turn.text
    }
}

/// The page the preview draws: the file the core wrote, and the page JSON
/// that redraws it in place.
struct SharePreviewFrame: Equatable {
    let html: URL
    let audio: String?
    let page: Data
    let generation: Int
}

/// One meeting's draft while the share sheet is open. Every edit is held
/// here, previewed and saved a quarter second after the last keystroke, so
/// closing the sheet keeps it.
@MainActor
@Observable
final class ShareSheetModel {
    enum Phase: Equatable {
        case loading
        case unreadable(String)
        case editing
        case publishing
        case published
        case failed(String)
    }

    let meetingId: String
    private(set) var phase: Phase = .loading
    private(set) var preparedBy: String?
    private(set) var audioAvailable = false
    private(set) var durationS: Double?

    var include = SharedInclude(
        title: true, summary: true, actionItems: true, decisions: true, notes: false, transcript: false, audio: false
    ) { didSet { edited() } }
    var speakers = SharedSpeakers.names { didSet { edited() } }
    var expiry = SharedExpiry.never { didSet { edited() } }
    var title = "" { didSet { edited() } }
    var summary: [String] = [] { didSet { edited() } }
    var decisions: [String] = [] { didSet { edited() } }
    var actionItems: [SharedActionItem] = [] { didSet { edited() } }
    var notes: [String] = [] { didSet { edited() } }
    var speakerNames: [String] = [] { didSet { edited() } }
    private(set) var turns: [ShareTurn] = []
    private(set) var frame: SharePreviewFrame?
    private(set) var previewFailure: String?
    private(set) var saveFailure: String?
    private(set) var record: SharedRecord?
    private(set) var stopFailure: String?
    private(set) var stopping = false

    @ObservationIgnored private let core: Core
    @ObservationIgnored private let shared: SharedPagesStore
    @ObservationIgnored private var base: SharedDraft?
    @ObservationIgnored private var speakerOrder: [Int: Int] = [:]
    @ObservationIgnored private var adopting = false
    @ObservationIgnored private var dirty = false
    @ObservationIgnored private var revision = 0
    @ObservationIgnored private var pending: Task<Void, Never>?

    init(meetingId: String, core: Core, shared: SharedPagesStore) {
        self.meetingId = meetingId
        self.core = core
        self.shared = shared
    }

    var editable: Bool {
        switch phase {
        case .editing, .failed: true
        default: false
        }
    }

    func load() async {
        phase = .loading
        do {
            let draft: SharedDraft = try await core.request(
                "shared_draft", CloudSyncRequest(request: ["meeting_id": meetingId]))
            adopt(draft)
            phase = .editing
            await sync()
        } catch {
            phase = .unreadable(SharedFailure.words(error))
        }
    }

    /// "Speaker 2" counts people in the order they first speak, the way
    /// the page numbers them.
    func speakerLabel(_ index: Int) -> String {
        let number = "Speaker \(speakerOrder[index] ?? index + 1)"
        guard speakers == .names, speakerNames.indices.contains(index) else { return number }
        let name = speakerNames[index].trimmingCharacters(in: .whitespaces)
        return name.isEmpty ? number : name
    }

    func edited() {
        guard !adopting, editable else { return }
        dirty = true
        pending?.cancel()
        pending = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            await self?.sync()
        }
    }

    /// Redraws the preview from the current edits, then saves them.
    func sync() async {
        guard let draft = current() else { return }
        dirty = false
        revision += 1
        let mine = revision
        do {
            let preview: SharedPreview = try await core.request(
                "shared_preview", CloudSyncRequest(request: SharedDraftBody(draft: draft)))
            if mine == revision {
                frame = SharePreviewFrame(
                    html: URL(fileURLWithPath: preview.htmlPath),
                    audio: preview.audioPath,
                    page: try JSONEncoder().encode(preview.page),
                    generation: mine)
                previewFailure = nil
            }
        } catch {
            if mine == revision {
                previewFailure = SharedFailure.words(error)
            }
        }
        await save(draft)
    }

    /// Saves what the sheet holds before it closes, when the last edit has
    /// not been saved yet.
    func flush() async {
        pending?.cancel()
        guard dirty, let draft = current() else { return }
        dirty = false
        await save(draft)
    }

    func publish() async {
        guard let draft = current() else { return }
        pending?.cancel()
        phase = .publishing
        do {
            let record: SharedRecord = try await core.request(
                "shared_publish", CloudSyncRequest(request: SharedDraftBody(draft: draft)))
            shared.replace(record)
            if record.phase == .failed {
                phase = .failed(record.error ?? "The page did not go live. Nothing was shared.")
            } else {
                self.record = record
                phase = .published
            }
        } catch {
            phase = .failed(SharedFailure.words(error))
            await shared.readSite()
        }
    }

    func stopSharing() async {
        guard let slug = record?.slug else { return }
        stopping = true
        defer { stopping = false }
        do {
            let record: SharedRecord = try await core.request(
                "shared_revoke", CloudSyncRequest(request: ["slug": slug]))
            shared.replace(record)
            self.record = record
            stopFailure = nil
        } catch {
            stopFailure = SharedFailure.words(error)
        }
    }

    private func save(_ draft: SharedDraft) async {
        do {
            let _: JSONValue = try await core.request(
                "shared_draft_save", CloudSyncRequest(request: SharedDraftBody(draft: draft)))
            saveFailure = nil
        } catch {
            saveFailure = "Edits are not saved: \(SharedFailure.words(error))"
        }
    }

    private func adopt(_ draft: SharedDraft) {
        adopting = true
        defer { adopting = false }
        base = draft
        preparedBy = draft.preparedBy.flatMap { $0.isEmpty ? nil : $0 }
        audioAvailable = draft.audioAvailable
        durationS = draft.content.durationS
        include = draft.include
        speakers = draft.speakers
        expiry = SharedExpiry(days: draft.expiresInDays)
        title = draft.content.title ?? ""
        summary = draft.content.summary
        decisions = draft.content.decisions
        actionItems = draft.content.actionItems
        notes = draft.content.notes
        speakerNames = draft.content.speakers
        turns = draft.content.transcript.enumerated().map { ShareTurn(id: $0.offset, turn: $0.element) }
        var order: [Int: Int] = [:]
        for turn in draft.content.transcript where order[turn.s] == nil {
            order[turn.s] = order.count + 1
        }
        speakerOrder = order
    }

    private func current() -> SharedDraft? {
        guard var draft = base else { return nil }
        draft.include = include
        draft.speakers = speakers
        draft.expiresInDays = expiry.days
        draft.content.title = title
        draft.content.summary = summary
        draft.content.decisions = decisions
        draft.content.actionItems = actionItems
        draft.content.notes = notes
        draft.content.speakers = speakerNames
        draft.content.transcript = turns.map { SharedTurn(s: $0.s, t: $0.t, text: $0.text) }
        return draft
    }
}

private struct SharedDraftBody: Encodable {
    let draft: SharedDraft
}

/// The share sheet: what goes on the page and its text on the left, the
/// page itself on the right, and one Share press that sends it.
struct ShareSheet: View {
    let meetingId: String
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var share: ShareSheetModel?

    var body: some View {
        VStack(spacing: 0) {
            header
            Hairline()
            if let share {
                HStack(spacing: 0) {
                    ShareSheetLeft(share: share)
                        .frame(width: 420)
                    Hairline(axis: .vertical)
                    SharePreviewPane(share: share)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                Spacer()
            }
            Hairline()
            if let share {
                ShareSheetFooter(share: share) { dismiss() }
            }
        }
        .frame(minWidth: 860, idealWidth: 1120, minHeight: 560, idealHeight: 760)
        .background(Theme.page)
        .task {
            let share = ShareSheetModel(meetingId: meetingId, core: model.core, shared: model.shared)
            self.share = share
            if !model.shared.siteRead {
                await model.shared.readSite()
            }
            await share.load()
        }
        .onDisappear {
            guard let share else { return }
            let shared = model.shared
            Task {
                await share.flush()
                await shared.refresh()
            }
        }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Share this meeting")
                    .titleText()
                    .accessibilityAddTraits(.isHeader)
                if let title = share?.title, !title.isEmpty {
                    Text(title).metaText(Theme.inkSecondary).lineLimit(1)
                }
            }
            Spacer()
            Button { dismiss() } label: {
                Image(systemName: "xmark")
            }
            .buttonStyle(.icon)
            .keyboardShortcut(.cancelAction)
            .help("Close")
            .accessibilityLabel("Close")
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 18)
    }
}

private struct ShareSheetLeft: View {
    let share: ShareSheetModel

    var body: some View {
        switch share.phase {
        case .loading:
            VStack(spacing: 10) {
                ProgressView().controlSize(.small)
                Text("Reading the meeting…").metaText()
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case let .unreadable(message):
            VStack(alignment: .leading, spacing: 12) {
                Text(message).bodyText(14, Theme.live).textSelection(.enabled)
                Button("Try again") { Task { await share.load() } }
                    .buttonStyle(.secondary)
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        case .published:
            SharePublishedPanel(share: share)
        case .editing, .publishing, .failed:
            ShareEditor(share: share)
                .disabled(!share.editable)
        }
    }
}

/// The left column: what goes on the page, then the text of each part that
/// does. The transcript's turns sit directly in the lazy stack, so only the
/// rows on screen exist.
private struct ShareEditor: View {
    @Bindable var share: ShareSheetModel
    @Environment(AppModel.self) private var model

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                if let preparedBy = share.preparedBy {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Image(systemName: "info.circle")
                            .foregroundStyle(Theme.accent)
                            .accessibilityHidden(true)
                        Text("Prepared by \(preparedBy). Check it before sharing.")
                            .bodyText(13)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.wash, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                    .padding(.bottom, 24)
                }
                if model.shared.siteRead, model.shared.site == nil {
                    PageSection("Where pages go") {
                        SharedSiteSetup()
                    }
                }
                PageSection("On the page") {
                    ShareParts(share: share)
                }
                PageSection("Link") {
                    Card {
                        ChoiceRow(
                            title: "Expires",
                            choices: SharedExpiry.allCases,
                            label: \.label,
                            selection: $share.expiry)
                    }
                }
                if share.include.title {
                    ShareEditorBlock("Title") {
                        ShareField(prompt: "Meeting title", text: $share.title)
                    }
                }
                if share.include.summary, !share.summary.isEmpty {
                    ShareEditorBlock("Summary") {
                        ShareParagraphs(prompt: "Empty paragraph", paragraphs: $share.summary)
                    }
                }
                if share.include.actionItems, !share.actionItems.isEmpty {
                    ShareEditorBlock("Action items") {
                        ShareActionItems(items: $share.actionItems)
                    }
                }
                if share.include.decisions, !share.decisions.isEmpty {
                    ShareEditorBlock("Decisions") {
                        ShareParagraphs(prompt: "Empty decision", paragraphs: $share.decisions)
                    }
                }
                if share.include.notes, !share.notes.isEmpty {
                    ShareEditorBlock("My notes") {
                        ShareParagraphs(prompt: "Empty note", paragraphs: $share.notes)
                    }
                }
                if share.include.transcript, !share.turns.isEmpty {
                    if share.speakers == .names, !share.speakerNames.isEmpty {
                        ShareEditorBlock("Speakers") {
                            ShareParagraphs(prompt: "Unnamed speaker", paragraphs: $share.speakerNames)
                        }
                    }
                    Text("Transcript")
                        .sectionLabel()
                        .accessibilityAddTraits(.isHeader)
                        .padding(.bottom, 10)
                    ShareTranscriptRows(share: share)
                }
            }
            .padding(24)
        }
    }
}

/// The switches for each part of the page, with what each part holds.
private struct ShareParts: View {
    @Bindable var share: ShareSheetModel

    var body: some View {
        Card {
            ToggleRow(title: "Title and date", isOn: $share.include.title)
            ToggleRow(title: "Summary", detail: count(share.summary.count, "paragraph"), isOn: $share.include.summary)
            ToggleRow(
                title: "Action items and owners", detail: count(share.actionItems.count, "item"),
                isOn: $share.include.actionItems)
            ToggleRow(title: "Decisions", detail: count(share.decisions.count, "decision"), isOn: $share.include.decisions)
            ToggleRow(title: "My notes", detail: count(share.notes.count, "paragraph"), isOn: $share.include.notes)
            ToggleRow(title: "Transcript", detail: count(share.turns.count, "turn"), isOn: $share.include.transcript)
            if share.include.transcript {
                ChoiceRow(
                    title: "Speakers",
                    detail: "Names written in the summary, notes and action item owners stay as they are.",
                    choices: SharedSpeakers.allCases,
                    label: \.label,
                    selection: $share.speakers)
            }
            ToggleRow(title: "Audio", detail: audioDetail, isOn: $share.include.audio)
                .disabled(!share.audioAvailable)
        }
    }

    private var audioDetail: String {
        guard share.audioAvailable else { return "The recording is not on this Mac" }
        guard let duration = share.durationS else { return "The meeting's recording" }
        return "\(((duration / 60).rounded() * 60).spoken) recording"
    }

    private func count(_ number: Int, _ noun: String) -> String {
        switch number {
        case 0: "None in this meeting"
        case 1: "1 \(noun)"
        default: "\(number.formatted()) \(noun)s"
        }
    }
}

private struct ShareEditorBlock<Content: View>: View {
    let label: String
    let content: Content

    init(_ label: String, @ViewBuilder content: () -> Content) {
        self.label = label
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(label).sectionLabel().accessibilityAddTraits(.isHeader)
            content
        }
        .padding(.bottom, 24)
    }
}

/// A text box that grows with what it holds.
private struct ShareField: View {
    let prompt: String
    @Binding var text: String
    @FocusState private var focused: Bool

    var body: some View {
        TextField(prompt, text: $text, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary), axis: .vertical)
            .textFieldStyle(.plain)
            .font(TypeScale.body(13))
            .foregroundStyle(Theme.ink)
            .focused($focused)
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .strokeBorder(focused ? Theme.accent : Theme.border, lineWidth: 1))
    }
}

private struct ShareParagraphs: View {
    let prompt: String
    @Binding var paragraphs: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(paragraphs.indices, id: \.self) { index in
                ShareField(prompt: prompt, text: $paragraphs[index])
            }
        }
    }
}

private struct ShareActionItems: View {
    @Binding var items: [SharedActionItem]

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(items.indices, id: \.self) { index in
                VStack(alignment: .leading, spacing: 4) {
                    ShareField(prompt: "Empty action item", text: $items[index].text)
                    HStack(spacing: 8) {
                        Text("Owner").metaText()
                        ShareField(prompt: "No owner", text: owner(index))
                    }
                }
            }
        }
    }

    private func owner(_ index: Int) -> Binding<String> {
        Binding(
            get: { items[index].owner ?? "" },
            set: { items[index].owner = $0.isEmpty ? nil : $0 })
    }
}

private struct ShareTranscriptRows: View {
    let share: ShareSheetModel

    var body: some View {
        ForEach(share.turns) { turn in
            ShareTurnRow(share: share, turn: turn)
        }
    }
}

private struct ShareTurnRow: View {
    let share: ShareSheetModel
    let turn: ShareTurn

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(turn.t.clock)
                .font(TypeScale.mono(11))
                .foregroundStyle(Theme.inkTertiary)
                .frame(width: 44, alignment: .leading)
            VStack(alignment: .leading, spacing: 4) {
                Text(share.speakerLabel(turn.s)).metaText(Theme.inkSecondary)
                ShareField(prompt: "Nothing said", text: text)
            }
        }
        .padding(.bottom, 10)
    }

    private var text: Binding<String> {
        Binding(
            get: { turn.text },
            set: { value in
                turn.text = value
                share.edited()
            })
    }
}

/// After Share: the link and what can be done with it.
private struct SharePublishedPanel: View {
    let share: ShareSheetModel
    @State private var copied = false
    @State private var confirming = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            if let record = share.record {
                let phase = record.phase
                VStack(alignment: .leading, spacing: 6) {
                    Text(headline(phase)).headlineText()
                    Text(explanation(record, phase))
                        .bodyText(14, Theme.inkSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let url = record.url, phase.stoppable {
                    HStack(spacing: 8) {
                        Text(url)
                            .font(TypeScale.mono(12))
                            .foregroundStyle(Theme.ink)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .textSelection(.enabled)
                        Spacer(minLength: 8)
                    }
                    .padding(.horizontal, 14)
                    .frame(height: 38)
                    .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                    HStack(spacing: 8) {
                        Button(copied ? "Link copied" : "Copy link") {
                            CloudSyncClipboard.copy(url)
                            copied = true
                        }
                        .buttonStyle(.secondary)
                        if let link = URL(string: url) {
                            Button("Open in browser") { NSWorkspace.shared.open(link) }
                                .buttonStyle(.secondary)
                        }
                        Spacer()
                        Button(share.stopping ? "Stopping…" : "Stop sharing") { confirming = true }
                            .buttonStyle(QuietButton(color: Theme.live))
                            .disabled(share.stopping)
                    }
                }
                if let failure = share.stopFailure {
                    Text(failure).metaText(Theme.live).textSelection(.enabled)
                }
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .alert("Stop sharing this page?", isPresented: $confirming) {
            Button("Cancel", role: .cancel) {}
            Button("Stop sharing", role: .destructive) {
                Task { await share.stopSharing() }
            }
        } message: {
            Text("The page goes offline within about a minute. People who already opened it may have kept a copy.")
        }
    }

    private func headline(_ phase: SharedState) -> String {
        switch phase {
        case .live: "The page is live"
        case .goingLive: "The page is on its way"
        case .removing: "Taking the page down"
        case .removed: "The page is offline"
        case .expired: "The link has expired"
        case .failed, .other: "The page did not go live"
        }
    }

    private func explanation(_ record: SharedRecord, _ phase: SharedState) -> String {
        switch phase {
        case .live:
            if let expiry = record.expiry {
                return "Anyone with the link can read it until \(expiry.short). Stop sharing takes it down sooner."
            }
            return "Anyone with the link can read it until you stop sharing."
        case .goingLive:
            return "GitHub is still putting it online. That usually takes under a minute, sometimes a few. The Shared list says when it is live."
        case .removing:
            return "It goes offline within about a minute."
        case .removed:
            return "The link no longer opens anything."
        case .expired:
            return "The link no longer opens anything."
        case .failed, .other:
            return record.error ?? "Nothing was shared."
        }
    }
}

private struct SharePreviewPane: View {
    let share: ShareSheetModel

    var body: some View {
        ZStack {
            Theme.inset
            if let frame = share.frame {
                SharePreviewWeb(frame: frame)
            } else if let failure = share.previewFailure {
                failed(failure)
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .overlay(alignment: .bottom) {
            if share.frame != nil, let failure = share.previewFailure {
                failed(failure)
                    .padding(12)
                    .frame(maxWidth: .infinity)
                    .background(Theme.surface)
            }
        }
    }

    private func failed(_ message: String) -> some View {
        HStack(spacing: 12) {
            Text("The preview could not be drawn. \(message)")
                .metaText(Theme.live)
                .fixedSize(horizontal: false, vertical: true)
            Button("Try again") { Task { await share.sync() } }
                .buttonStyle(.compact)
        }
        .padding(16)
    }
}

private struct ShareSheetFooter: View {
    let share: ShareSheetModel
    let close: () -> Void
    @Environment(AppModel.self) private var model

    private var siteMissing: Bool { model.shared.siteRead && model.shared.site == nil }

    var body: some View {
        HStack(spacing: 12) {
            status
            Spacer(minLength: 12)
            switch share.phase {
            case .published:
                Button("Done", action: close)
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
            case .failed:
                Button("Try again") { Task { await share.publish() } }
                    .buttonStyle(.primary)
                    .disabled(siteMissing)
            default:
                Button(share.phase == .publishing ? "Sharing…" : "Share") { Task { await share.publish() } }
                    .buttonStyle(.primary)
                    .disabled(share.phase != .editing || siteMissing || share.frame == nil)
            }
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 16)
    }

    @ViewBuilder
    private var status: some View {
        switch share.phase {
        case .publishing:
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Putting the page online. This usually takes under a minute.").metaText(Theme.inkSecondary)
            }
        case let .failed(message):
            Text(message)
                .metaText(Theme.live)
                .lineLimit(3)
                .textSelection(.enabled)
        case .published:
            EmptyView()
        default:
            if siteMissing {
                Text("Add where pages go before sharing. Nothing leaves this Mac until you press Share.")
                    .metaText(Theme.inkSecondary)
            } else if let failure = share.saveFailure {
                Text(failure).metaText(Theme.live).lineLimit(2)
            } else {
                Text("Nothing leaves this Mac until you press Share.").metaText(Theme.inkSecondary)
            }
        }
    }
}

/// The preview: the core's page file, redrawn in place with each new page
/// so the reader's scroll stays put. A change to the audio reloads it,
/// since the file names the recording only when it is written.
private struct SharePreviewWeb: NSViewRepresentable {
    let frame: SharePreviewFrame

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> WKWebView {
        let view = WKWebView(frame: .zero, configuration: WKWebViewConfiguration())
        view.navigationDelegate = context.coordinator
        view.allowsBackForwardNavigationGestures = false
        return view
    }

    func updateNSView(_ view: WKWebView, context: Context) {
        context.coordinator.show(frame, in: view)
    }

    static func dismantleNSView(_ view: WKWebView, coordinator: Coordinator) {
        view.stopLoading()
        view.navigationDelegate = nil
    }

    /// Loads the page file once and redraws it after; nothing else may
    /// load, since the page's own links point at the site.
    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate {
        private var document: String?
        private var file: URL?
        private var shown = -1
        private var loading = false
        private var waiting: SharePreviewFrame?

        func show(_ frame: SharePreviewFrame, in view: WKWebView) {
            guard frame.generation != shown else { return }
            shown = frame.generation
            let document = frame.html.path + "\n" + (frame.audio ?? "")
            if document != self.document {
                self.document = document
                file = frame.html
                loading = true
                waiting = nil
                view.loadFileURL(frame.html, allowingReadAccessTo: frame.html.deletingLastPathComponent())
            } else if loading {
                waiting = frame
            } else {
                render(frame, in: view)
            }
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            loading = false
            if let waiting {
                self.waiting = nil
                render(waiting, in: webView)
            }
        }

        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            loading = false
        }

        func webView(
            _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction
        ) async -> WKNavigationActionPolicy {
            guard navigationAction.navigationType != .linkActivated,
                let url = navigationAction.request.url, url.isFileURL, url.path == file?.path
            else { return .cancel }
            return .allow
        }

        private func render(_ frame: SharePreviewFrame, in view: WKWebView) {
            guard let page = try? JSONSerialization.jsonObject(with: frame.page) else { return }
            view.callAsyncJavaScript(
                "window.sonaRender(page)", arguments: ["page": page], in: nil, in: .page, completionHandler: nil)
        }
    }
}
