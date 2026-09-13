import SwiftUI

/// The agent panel's settings: the switch, where the panel sends its turns,
/// and the key it will believe when they come back.
///
/// Three fields and two buttons, because pairing is three facts and one
/// question the fields cannot answer: does it answer. The three facts are
/// written when you are done with a field — Return, or moving on — and the
/// row at the top says whether this Mac is paired, which is the receipt.
/// Saving and testing stay separate: a relay that is asleep is still the
/// relay you paired with, and a screen that refuses to keep an address it
/// cannot reach right now is a screen that cannot be used on a laptop.
///
/// The private half of this Mac's identity never appears here. It lives in
/// the keychain; the public half is shown so it can be added to the relay's
/// allowlist, which is the other half of the handshake and the one thing a
/// reader has to carry out of this screen by hand.
struct AgentPairingView: View {
    let store: AgentPairingStore
    @FocusState private var focus: AgentPairingField?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ErrorNote(store.error)

            PageSection("Sona agent") {
                Card {
                    ToggleRow(
                        title: "Enable agent panel",
                        detail: "Turning this off closes the panel and prevents other apps "
                            + "from opening it again.",
                        isOn: enabled)
                    pairing
                    lastTested
                }
                if store.reached, store.error == nil {
                    Text("The server replied.")
                        .metaText(Theme.inkSecondary)
                        .padding(.top, 10)
                }
            }

            PageSection("Where it sends turns") {
                Card {
                    AgentPairingInput(
                        label: "Server address",
                        hint: "Only a Tailscale address or this Mac itself is accepted. "
                            + "Anything on the open internet is refused before it is saved.",
                        prompt: "http://100.64.0.1:8650",
                        text: Binding(get: { store.relayUrl }, set: { store.relayUrl = $0 }),
                        field: .relayUrl,
                        focus: $focus,
                        isEnabled: !store.isBusy,
                        commit: commit)
                    AgentPairingInput(
                        label: "Server key ID",
                        text: Binding(get: { store.relayKeyId }, set: { store.relayKeyId = $0 }),
                        field: .relayKeyId,
                        focus: $focus,
                        isEnabled: !store.isBusy,
                        commit: commit)
                    AgentPairingInput(
                        label: "Server public key",
                        mono: true,
                        text: Binding(
                            get: { store.relayPublicKey }, set: { store.relayPublicKey = $0 }),
                        field: .relayPublicKey,
                        focus: $focus,
                        isEnabled: !store.isBusy,
                        commit: commit)
                    identity
                }
            }
        }
        .task { await store.start() }
        /* Moving on from a field writes the three of them, because a pairing
         * is one fact in three parts and the core takes it whole. Nothing is
         * written until all three are filled in and at least one differs from
         * what is stored, so tabbing through a saved pairing is silent and a
         * half-filled one waits. */
        .onChange(of: focus) { previous, _ in
            if previous != nil { store.save() }
        }
    }

    /// The row that says whether this Mac is paired, and the three things
    /// that can be done about it.
    private var pairing: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Pairing").bodyText()
                Text(store.isPaired ? "Paired" : "Not paired").metaText()
            }
        } trailing: {
            HStack(spacing: 8) {
                Button("Save", action: commit)
                    .buttonStyle(.compact)
                    .disabled(!store.canSave)
                Button("Test") { store.test() }
                    .buttonStyle(.compact)
                    .disabled(!store.canTest)
                Button("Unpair") { store.clear() }
                    .buttonStyle(.quiet)
                    .disabled(!store.canClear)
            }
        }
    }

    /// The stored stamp is written in exactly one place — the Test button's
    /// command — so it is the last successful test, not the last turn.
    private var lastTested: some View {
        CardRow {
            Text("Last tested").bodyText()
        } trailing: {
            Text(stamp).metaText(Theme.inkSecondary)
        }
    }

    private var stamp: String {
        guard let date = store.lastTested else { return "Never" }
        return "\(date.short), \(date.time)"
    }

    private var identity: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 8) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("This Mac's public key").bodyText()
                    Text(
                        "Add this key to the server so it accepts messages from this Mac. "
                            + "The private key never leaves your keychain."
                    )
                    .metaText()
                    .fixedSize(horizontal: false, vertical: true)
                }
                HStack(spacing: 10) {
                    Text(store.identity?.publicKey ?? "Turn the agent panel on to create a key.")
                        .font(
                            store.identity == nil ? TypeScale.body(13) : TypeScale.mono(13)
                        )
                        .foregroundStyle(Theme.inkSecondary)
                        .lineLimit(1)
                        /* A key is checked end by end against the one in the
                         * relay's allowlist, so both ends stay on screen. */
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                    Spacer(minLength: 8)
                    Button(store.copied ? "Copied" : "Copy") { store.copyIdentity() }
                        .buttonStyle(.compact)
                        .disabled(store.identity == nil)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var enabled: Binding<Bool> {
        Binding(get: { store.isEnabled }, set: { store.setEnabled($0) })
    }

    /// Return and the Save button are the same act as moving on.
    private func commit() {
        focus = nil
        store.save()
    }
}

/// Which of the three parts of a pairing has the caret.
private enum AgentPairingField: Hashable {
    case relayUrl, relayKeyId, relayPublicKey
}

/// One part of the pairing: what it is, what is accepted, and the field.
///
/// The field is inside the row rather than beside it because a server address
/// and a public key are both too long to read in the width a trailing control
/// gets, and a key that wraps mid-token is a key nobody can check.
private struct AgentPairingInput: View {
    let label: String
    var hint: String?
    var prompt = ""
    var mono = false
    @Binding var text: String
    let field: AgentPairingField
    @FocusState.Binding var focus: AgentPairingField?
    let isEnabled: Bool
    let commit: () -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 8) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(label).bodyText()
                    if let hint {
                        Text(hint)
                            .metaText()
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                TextField(
                    label,
                    text: $text,
                    prompt: prompt.isEmpty
                        ? nil : Text(prompt).foregroundStyle(Theme.inkTertiary)
                )
                .textFieldStyle(.plain)
                .labelsHidden()
                .font(mono ? TypeScale.mono(13) : TypeScale.body())
                .foregroundStyle(Theme.ink)
                .autocorrectionDisabled()
                .focused($focus, equals: field)
                .disabled(!isEnabled)
                .onSubmit(commit)
                .padding(.horizontal, 12)
                .frame(height: 36)
                .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.radiusControl)
                        .strokeBorder(focus == field ? Theme.accent : Theme.border, lineWidth: 1))
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
