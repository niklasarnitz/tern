import Foundation

struct ComposeDraft: Identifiable {
    let id = UUID()
    var recipients: [String]
    var carbonCopyRecipients: [String]
    var blindCarbonCopyRecipients: [String]
    var subject: String
    var body: String
    var attachments: [URL]

    static var empty: ComposeDraft {
        ComposeDraft(
            recipients: [],
            carbonCopyRecipients: [],
            blindCarbonCopyRecipients: [],
            subject: "",
            body: "",
            attachments: []
        )
    }
}

enum MailtoURLParser {
    static func parse(_ url: URL) -> ComposeDraft? {
        guard url.scheme?.lowercased() == "mailto",
              let colon = url.absoluteString.firstIndex(of: ":") else { return nil }

        let resource = url.absoluteString[url.absoluteString.index(after: colon)...]
        let withoutFragment = resource.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false)[0]
        let parts = withoutFragment.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
        guard let recipients = decodeRecipients(parts[0]) else { return nil }

        var draft = ComposeDraft.empty
        draft.recipients = recipients

        if parts.count == 2, !parseHeaders(parts[1], into: &draft) {
            return nil
        }
        return draft
    }

    static func recipients(from text: String) -> [String] {
        text.split(separator: ",", omittingEmptySubsequences: false)
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }

    private static func parseHeaders(_ query: Substring, into draft: inout ComposeDraft) -> Bool {
        var hasSubject = false
        var hasBody = false
        for field in query.split(separator: "&", omittingEmptySubsequences: false) where !field.isEmpty {
            let parts = field.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            guard let name = decode(parts[0])?.lowercased() else { return false }
            let rawValue = parts.count == 2 ? parts[1] : Substring()

            switch name {
            case "to", "cc", "bcc":
                guard let recipients = decodeRecipients(rawValue) else { return false }
                append(recipients, for: name, to: &draft)
            case "subject" where !hasSubject:
                guard let value = decode(rawValue) else { return false }
                draft.subject = value
                hasSubject = true
            case "body" where !hasBody:
                guard let value = decode(rawValue) else { return false }
                draft.body = value
                hasBody = true
            case "attach", "attachment":
                guard let value = decode(rawValue) else { return false }
                if let attachment = localAttachment(from: value) {
                    draft.attachments.append(attachment)
                }
            default:
                continue
            }
        }
        return true
    }

    private static func append(_ recipients: [String], for field: String, to draft: inout ComposeDraft) {
        switch field {
        case "to":
            draft.recipients.append(contentsOf: recipients)
        case "cc":
            draft.carbonCopyRecipients.append(contentsOf: recipients)
        default:
            draft.blindCarbonCopyRecipients.append(contentsOf: recipients)
        }
    }

    private static func decodeRecipients(_ value: Substring) -> [String]? {
        let encodedRecipients = value.split(separator: ",", omittingEmptySubsequences: false)
        var recipients: [String] = []
        for encodedRecipient in encodedRecipients {
            guard let decoded = decode(encodedRecipient) else { return nil }
            let trimmed = decoded.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                recipients.append(trimmed)
            }
        }
        return recipients
    }

    private static func decode(_ value: Substring) -> String? {
        String(value).removingPercentEncoding
    }

    private static func localAttachment(from value: String) -> URL? {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if let url = URL(string: trimmed), url.isFileURL {
            guard url.host == nil || url.host?.isEmpty == true || url.host == "localhost" else { return nil }
            return url.standardizedFileURL
        }
        guard trimmed.hasPrefix("/") else { return nil }
        return URL(fileURLWithPath: trimmed).standardizedFileURL
    }
}
