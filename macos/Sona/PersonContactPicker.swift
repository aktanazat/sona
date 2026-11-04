import Contacts
import ContactsUI
import SwiftUI

/// The Contacts picker, for attaching one contact to one person.
///
/// The contact the user picks here is the only way one reaches Sona. Nothing
/// in this file reads the address book, asks for access to it, or matches a
/// contact to a person by name.
@MainActor
final class PersonContactPicker: NSObject, @preconcurrency CNContactPickerDelegate {
    /// The view the picker's popover points at.
    weak var anchor: NSView?
    private var picker: CNContactPicker?
    private var chosen: (@MainActor (PersonContactCard?) -> Void)?
    private var fallbackName = ""

    /// Shows the picker under the anchor. `chosen` runs once, only if the user
    /// picks a contact, and with nil when that contact holds nothing beyond a
    /// name.
    func present(fallbackName: String, chosen: @escaping @MainActor (PersonContactCard?) -> Void) {
        guard picker == nil, let anchor else { return }
        let picker = CNContactPicker()
        picker.delegate = self
        picker.displayedKeys = []
        self.picker = picker
        self.chosen = chosen
        self.fallbackName = fallbackName
        picker.showRelative(to: anchor.bounds, of: anchor, preferredEdge: .minY)
    }

    func contactPicker(_ picker: CNContactPicker, didSelect contact: CNContact) {
        let chosen = chosen
        let card = PersonContactCard(contact, fallbackName: fallbackName)
        finish()
        picker.close()
        chosen?(card)
    }

    func contactPickerDidClose(_ picker: CNContactPicker) {
        finish()
    }

    private func finish() {
        picker = nil
        chosen = nil
    }
}

/// Marks the view the contact picker's popover points at.
struct PersonContactPickerAnchor: NSViewRepresentable {
    let picker: PersonContactPicker

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        picker.anchor = view
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        picker.anchor = view
    }
}

extension PersonContactCard {
    /// What one picked contact says, inside the limits the core keeps. A
    /// field the picker did not hand over is skipped, never fetched: reading
    /// a key that was not handed over raises. Nil when nothing beyond a name
    /// is left, since a name alone adds nothing the page lacks.
    init?(_ contact: CNContact, fallbackName: String) {
        let has = { (key: String) in contact.isKeyAvailable(key) }
        let fullName = contact.areKeysAvailable([CNContactFormatter.descriptorForRequiredKeys(for: .fullName)])
            ? CNContactFormatter.string(from: contact, style: .fullName).flatMap { Self.line($0, limit: 256) }
            : nil
        let organization = has(CNContactOrganizationNameKey) ? Self.line(contact.organizationName, limit: 256) : nil
        let jobTitle = has(CNContactJobTitleKey) ? Self.line(contact.jobTitle, limit: 256) : nil
        let emails = has(CNContactEmailAddressesKey)
            ? Self.values(contact.emailAddresses) { Self.email($0 as String) }
            : []
        let phones = has(CNContactPhoneNumbersKey)
            ? Self.values(contact.phoneNumbers) { Self.line($0.stringValue, limit: 64) }
            : []
        let formatter = CNPostalAddressFormatter()
        let addresses = has(CNContactPostalAddressesKey)
            ? Self.values(contact.postalAddresses) { Self.address(formatter.string(from: $0)) }
            : []
        let urls = has(CNContactUrlAddressesKey)
            ? Self.values(contact.urlAddresses) { Self.web($0 as String) }
            : []
        let birthday = has(CNContactBirthdayKey) ? contact.birthday.flatMap(Self.birthday) : nil
        guard organization != nil || jobTitle != nil || !emails.isEmpty || !phones.isEmpty
            || !addresses.isEmpty || !urls.isEmpty || birthday != nil
        else { return nil }
        self.init(
            name: fullName ?? organization ?? fallbackName,
            emails: emails, phones: phones, organization: organization, jobTitle: jobTitle,
            addresses: addresses, urls: urls, birthday: birthday)
    }

    /// Labeled values with Contacts' own label words, at most sixteen, each
    /// one dropped when `clean` refuses it.
    private static func values<Value: NSCopying & NSSecureCoding>(
        _ labeled: [CNLabeledValue<Value>], clean: (Value) -> String?
    ) -> [PersonContactValue] {
        let kept = labeled.compactMap { item -> PersonContactValue? in
            guard let value = clean(item.value) else { return nil }
            let label = item.label
                .map { CNLabeledValue<NSString>.localizedString(forLabel: $0) }
                .flatMap { line($0, limit: 64) }
            return PersonContactValue(label: label, value: value)
        }
        return Array(kept.prefix(16))
    }

    private static func line(_ text: String, limit: Int) -> String? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text.unicodeScalars.count <= limit,
              !text.unicodeScalars.contains(where: { $0.properties.generalCategory == .control })
        else { return nil }
        return text
    }

    private static func email(_ text: String) -> String? {
        guard let text = line(text, limit: 256), text.contains("@"),
              !text.contains(where: \.isWhitespace) else { return nil }
        return text
    }

    private static func address(_ text: String) -> String? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text.unicodeScalars.count <= 512,
              !text.unicodeScalars.contains(where: { $0 != "\n" && $0.properties.generalCategory == .control })
        else { return nil }
        return text
    }

    /// An http or https address with a host and no account in it; one typed
    /// without a scheme gets https.
    private static func web(_ text: String) -> String? {
        guard let text = line(text, limit: 2048) else { return nil }
        let full = text.contains("://") ? text : "https://\(text)"
        guard let parts = URLComponents(string: full),
              let scheme = parts.scheme?.lowercased(), scheme == "http" || scheme == "https",
              let host = parts.host, !host.isEmpty, parts.user == nil, parts.password == nil,
              let url = parts.url?.absoluteString, url.utf8.count <= 2048
        else { return nil }
        return url
    }

    private static func birthday(_ parts: DateComponents) -> PersonContactBirthday? {
        guard let month = parts.month, let day = parts.day else { return nil }
        let year = parts.year.flatMap { (1...9999).contains($0) ? $0 : nil }
        var check = DateComponents()
        check.year = year ?? 2000
        check.month = month
        check.day = day
        guard (1...12).contains(month), (1...31).contains(day),
              check.isValidDate(in: Calendar(identifier: .gregorian))
        else { return nil }
        return PersonContactBirthday(month: UInt8(month), day: UInt8(day), year: year.map { Int32($0) })
    }
}
