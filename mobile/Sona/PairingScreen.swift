import CoreImage.CIFilterBuiltins
import SwiftUI

/// The one sheet behind the unpaired line: the phone publishes a candidate record, the
/// Mac approves it, the phone reads the approval back.
struct PairingScreen: View {
    @ObservedObject var model: AppModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    field("pair.address", text: $model.endpointDraft, identifier: "endpoint")
                        .keyboardType(.URL)
                    field("pair.vaultId", text: $model.vaultIdDraft, identifier: "vault-id")
                } footer: {
                    Text("pair.where")
                        .foregroundStyle(Theme.textSecondary)
                }
                .listRowBackground(Theme.surface)
                if let offer = model.pairingOffer {
                    code(offer)
                }
                Section {
                    actions
                } footer: {
                    if let message = model.pairingMessage {
                        Text(message)
                            .foregroundStyle(Theme.textSecondary)
                            .padding(.top, 4)
                            .accessibilityIdentifier("pair-message")
                    }
                }
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets())
                Section {
                    calls
                }
                .listRowBackground(Theme.surface)
            }
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .navigationTitle("pair.title")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button { dismiss() } label: {
                        Label("pair.close", systemImage: "xmark")
                    }
                }
            }
        }
        .tint(Theme.accent)
    }

    private func field(
        _ prompt: LocalizedStringKey, text: Binding<String>, identifier: String
    ) -> some View {
        TextField("", text: text, prompt: Text(prompt).foregroundStyle(Theme.textTertiary))
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .font(.body)
            .foregroundStyle(Theme.textPrimary)
            .frame(minHeight: 44)
            .accessibilityLabel(Text(prompt))
            .accessibilityIdentifier(identifier)
    }

    private func code(_ offer: PairingOffer) -> some View {
        Section {
            if let image = qrImage(offer.json) {
                /* Bounded, not `.infinity`: a full-width code pushes the button that
                 * finishes pairing off a phone screen. */
                Image(uiImage: image)
                    .interpolation(.none)
                    .resizable()
                    .frame(width: 180, height: 180)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 8)
                    .accessibilityLabel(Text("a11y.qr"))
            }
            LabeledContent("pair.fingerprint") {
                Text(offer.fingerprint)
                    .font(.footnote.monospaced())
                    .foregroundStyle(Theme.textPrimary)
            }
            .foregroundStyle(Theme.textPrimary)
            Button("pair.copy") { UIPasteboard.general.string = offer.json }
                .accessibilityIdentifier("copy-code")
        } header: {
            Text("pair.show")
                .font(.footnote)
                .foregroundStyle(Theme.textSecondary)
                .textCase(nil)
        }
        .listRowBackground(Theme.surface)
    }

    /// One switch, because the offer is either wanted or it is not.
    private var calls: some View {
        Toggle(isOn: Binding(get: { model.offersAfterCalls }, set: { model.offersAfterCalls = $0 })) {
            VStack(alignment: .leading, spacing: 2) {
                Text("call.toggle")
                    .font(.body)
                    .foregroundStyle(Theme.textPrimary)
                Text("call.toggle.detail")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }
        }
        .tint(Theme.accent)
        .accessibilityIdentifier("call-offers")
    }

    private var actions: some View {
        VStack(spacing: 12) {
            Button {
                Task { await model.createPairingCode() }
            } label: {
                Text("pair.create")
                    .font(.body.weight(.semibold))
                    .frame(maxWidth: .infinity, minHeight: 36)
            }
            .buttonStyle(.borderedProminent)
            .accessibilityIdentifier("create-pairing-code")
            Button {
                Task { await model.finishPairing() }
            } label: {
                Text("pair.finish")
                    .font(.body.weight(.semibold))
                    .frame(maxWidth: .infinity, minHeight: 36)
            }
            .buttonStyle(.bordered)
            .disabled(model.pairingOffer == nil)
            .accessibilityIdentifier("finish-pairing")
        }
        .buttonBorderShape(.capsule)
        .controlSize(.large)
    }

    private func qrImage(_ text: String) -> UIImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let scaled = output.transformed(by: CGAffineTransform(scaleX: 8, y: 8))
        let context = CIContext()
        guard let cgImage = context.createCGImage(scaled, from: scaled.extent) else { return nil }
        return UIImage(cgImage: cgImage)
    }
}
