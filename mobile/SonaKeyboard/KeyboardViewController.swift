import SwiftUI
import UIKit

@MainActor
private final class KeyboardState: ObservableObject {
    @Published var draft: KeyboardDraft?
    @Published var notice: String?
    @Published var hasFullAccess = false
}

@MainActor
final class KeyboardViewController: UIInputViewController {
    private let state = KeyboardState()
    private var minimumHeight: NSLayoutConstraint?

    /* A custom keyboard is given the height it asks for, and in landscape the whole screen
     * is shorter than a comfortable portrait keyboard, so a fixed floor would be a
     * required constraint the screen cannot satisfy. */
    private var wantedHeight: CGFloat {
        traitCollection.verticalSizeClass == .compact ? 200 : 280
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        let content = KeyboardView(
            state: state,
            refresh: { [weak self] in self?.reloadDraft() },
            insert: { [weak self] in self?.insertDraft() },
            discard: { [weak self] in self?.discardDraft() },
            nextKeyboard: { [weak self] in self?.advanceToNextInputMode() },
            space: { [weak self] in self?.textDocumentProxy.insertText(" ") },
            delete: { [weak self] in self?.textDocumentProxy.deleteBackward() },
            newLine: { [weak self] in self?.textDocumentProxy.insertText("\n") }
        )
        let host = UIHostingController(rootView: content)
        addChild(host)
        host.view.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(host.view)
        let minimum = view.heightAnchor.constraint(greaterThanOrEqualToConstant: wantedHeight)
        minimumHeight = minimum
        NSLayoutConstraint.activate([
            host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            host.view.topAnchor.constraint(equalTo: view.topAnchor),
            host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            minimum,
        ])
        host.didMove(toParent: self)
    }

    override func viewWillLayoutSubviews() {
        super.viewWillLayoutSubviews()
        /* Only on a real change: assigning a constant invalidates layout. */
        if let minimumHeight, minimumHeight.constant != wantedHeight {
            minimumHeight.constant = wantedHeight
        }
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        reloadDraft()
    }

    private func reloadDraft() {
        state.hasFullAccess = hasFullAccess
        guard hasFullAccess else {
            state.draft = nil
            state.notice = nil
            return
        }
        do {
            switch try KeyboardDraftStore.shared().load() {
            case .waiting(let draft):
                state.draft = draft
                state.notice = nil
            case .expired:
                state.draft = nil
                state.notice = KeyboardDraftError.expired.localizedDescription
            case .missing:
                state.draft = nil
                state.notice = nil
            }
        } catch {
            state.draft = nil
            state.notice = error.localizedDescription
        }
    }

    private func insertDraft() {
        /* Full Access can be revoked while this keyboard is on screen, and the reload is
         * what turns the stale buttons back into the explanation. */
        guard hasFullAccess, let draft = state.draft else { return reloadDraft() }
        do {
            /* Taken before it is inserted: a draft that was consumed but not inserted
             * costs the user a re-dictation, one inserted twice corrupts their message. */
            let text = try KeyboardDraftStore.shared().take(id: draft.id)
            state.draft = nil
            textDocumentProxy.insertText(text)
            state.notice = NSLocalizedString("keyboard.inserted", comment: "")
        } catch {
            reloadDraft()
            state.notice = error.localizedDescription
        }
    }

    private func discardDraft() {
        guard hasFullAccess, let draft = state.draft else { return reloadDraft() }
        do {
            try KeyboardDraftStore.shared().discard(id: draft.id)
            reloadDraft()
        } catch {
            reloadDraft()
            state.notice = error.localizedDescription
        }
    }
}

private struct KeyboardView: View {
    @ObservedObject var state: KeyboardState
    let refresh: () -> Void
    let insert: () -> Void
    let discard: () -> Void
    let nextKeyboard: () -> Void
    let space: () -> Void
    let delete: () -> Void
    let newLine: () -> Void

    var body: some View {
        VStack(spacing: 12) {
            /* Scrolled rather than compressed: in landscape the draft and its two buttons
             * do not fit above the keys, and an unreachable Insert is not a keyboard. */
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Text("keyboard.title").font(.headline)
                        Spacer()
                        Button(action: refresh) {
                            Label("keyboard.refresh", systemImage: "arrow.clockwise")
                                .labelStyle(.iconOnly)
                                .frame(minWidth: 44, minHeight: 44)
                        }
                        .accessibilityIdentifier("keyboard-refresh")
                    }
                    if !state.hasFullAccess {
                        Text("keyboard.needsFullAccess")
                            .font(.footnote)
                    } else if let draft = state.draft {
                        Text(draft.text)
                            .font(.body)
                            .lineLimit(1...3)
                            .accessibilityIdentifier("keyboard-preview")
                        HStack {
                            Button("keyboard.insert", action: insert)
                                .buttonStyle(.borderedProminent)
                                .fixedSize()
                                .accessibilityIdentifier("keyboard-insert")
                            Button("keyboard.discard", action: discard)
                                .fixedSize()
                                .accessibilityIdentifier("keyboard-discard")
                        }
                        .frame(minHeight: 44)
                    } else {
                        Text(state.notice ?? NSLocalizedString("keyboard.empty", comment: ""))
                            .font(.footnote)
                    }
                    /* Also without Full Access: that is the state where the only way
                     * forward is the app's own screen. */
                    Link("keyboard.openApp", destination: DictationLink.url)
                        .frame(minHeight: 44)
                        .accessibilityIdentifier("keyboard-open-sona")
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .scrollBounceBehavior(.basedOnSize)
            HStack(spacing: 12) {
                Button(action: nextKeyboard) {
                    Label("keyboard.next", systemImage: "globe")
                        .labelStyle(.iconOnly)
                        .frame(minWidth: 44, minHeight: 44)
                }
                Button("keyboard.space", action: space)
                    .frame(maxWidth: .infinity, minHeight: 44)
                Button(action: delete) {
                    Label("keyboard.delete", systemImage: "delete.left")
                        .labelStyle(.iconOnly)
                        .frame(minWidth: 44, minHeight: 44)
                }
                Button(action: newLine) {
                    Label("keyboard.return", systemImage: "return")
                        .labelStyle(.iconOnly)
                        .frame(minWidth: 44, minHeight: 44)
                }
            }
            .buttonStyle(.bordered)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .foregroundStyle(Theme.textPrimary)
        .background(Theme.inset)
        .tint(Theme.accent)
    }
}
