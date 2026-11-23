import SwiftUI

struct QuicklinksView: View {
    let store: QuicklinksStore
    let edit: (UUID?) -> Void
    let open: (UUID) -> Void
    @State private var deleting: Quicklink?
    @State private var error: String?

    var body: some View {
        Page {
            PageTitle("Quicklinks", subtitle: "") {
                Button("Add Quicklink") { edit(nil) }
                    .buttonStyle(.primary)
                    .disabled(!ready)
            }
            ErrorNote(error ?? store.openError)
            switch store.state {
            case .waiting, .loading:
                ProgressView().controlSize(.small).accessibilityLabel("Loading Quicklinks")
            case .unavailable:
                ActionRow(title: "Quicklinks couldn’t be read.", button: "Try again") {
                    Task { await store.load() }
                }
            case let .ready(_, links):
                if links.isEmpty {
                    Text("No quicklinks yet.").bodyText(14, Theme.inkSecondary)
                } else {
                    Card {
                        ForEach(links) { link in
                            QuicklinkRow(link: link, open: { open(link.id) },
                                         edit: { edit(link.id) }, delete: { deleting = link })
                        }
                    }
                }
            }
        }
        .alert("Delete quicklink?", isPresented: Binding(
            get: { deleting != nil }, set: { if !$0 { deleting = nil } })) {
            Button("Cancel", role: .cancel) {}
            Button("Delete", role: .destructive) {
                guard let deleting else { return }
                do {
                    try store.remove(deleting.id)
                    error = nil
                } catch {
                    self.error = error.localizedDescription
                }
            }
        } message: {
            if let deleting { Text(deleting.name) }
        }
    }

    private var ready: Bool {
        if case .ready = store.state { return true }
        return false
    }
}

private struct QuicklinkRow: View {
    let link: Quicklink
    let open: () -> Void
    let edit: () -> Void
    let delete: () -> Void

    var body: some View {
        CardRow(action: open) {
            VStack(alignment: .leading, spacing: 3) {
                Text(link.name).bodyText().lineLimit(1)
                Text(link.host).metaText().lineLimit(1)
            }
        } trailing: {
            Menu {
                Button("Edit", action: edit)
                Button("Delete", role: .destructive, action: delete)
            } label: {
                Image(systemName: "ellipsis")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .accessibilityLabel("Actions for \(link.name)")
        }
    }
}

@MainActor
struct QuicklinkEditor: View {
    let store: QuicklinksStore
    let id: UUID?
    private let original: Quicklink?
    @Environment(\.dismiss) private var dismiss
    @State private var name: String
    @State private var address: String
    @State private var error: String?
    @FocusState private var focused: Bool

    init(store: QuicklinksStore, id: UUID?) {
        self.store = store
        self.id = id
        let original = store.links.first { $0.id == id }
        self.original = original
        _name = State(initialValue: original?.name ?? "")
        _address = State(initialValue: original?.address ?? "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            Text(id == nil ? "Add Quicklink" : "Edit Quicklink").bodyText(18)
            if id == nil || original != nil {
                TextField("Name", text: $name).textFieldStyle(.roundedBorder).focused($focused)
                TextField("Web address", text: $address, prompt: Text("https://example.com"))
                    .textFieldStyle(.roundedBorder)
                Text("For a search, use a query value such as ?q={argument}.").metaText()
                ErrorNote(error)
            } else {
                ErrorNote(QuicklinkError.changed.localizedDescription)
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Save") { save() }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
                    .disabled(id != nil && original == nil)
            }
        }
        .padding(Space.l)
        .frame(width: 420)
        .background(Theme.page)
        .onAppear { focused = true }
    }

    private func save() {
        do {
            try store.save(name: name, address: address, replacing: original)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

struct QuicklinkArgumentSheet: View {
    let link: Quicklink?
    let open: (String) throws -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var argument = ""
    @State private var error: String?
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            if let link {
                Text(link.name).bodyText(18)
                Text(link.host).metaText()
                TextField("Search value", text: $argument).textFieldStyle(.roundedBorder).focused($focused)
                ErrorNote(error)
            } else {
                ErrorNote(QuicklinkError.changed.localizedDescription)
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Open") { launch() }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
                    .disabled(link == nil)
            }
        }
        .padding(Space.l)
        .frame(width: 420)
        .background(Theme.page)
        .onAppear { focused = true }
    }

    private func launch() {
        do {
            try open(argument)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
