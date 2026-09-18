import Foundation

struct SenderIdentity: Equatable {
    let displayName: String
    let address: String?
    let domain: String?
    let mismatchWarning: String?

    init(_ rawValue: String) {
        let value = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let parsed = Self.parse(value)
        displayName = parsed.name
        address = parsed.address
        domain = parsed.address.flatMap(Self.emailDomain)
        mismatchWarning = Self.mismatchWarning(displayName: parsed.name, actualDomain: domain)
    }

    private static func parse(_ value: String) -> (name: String, address: String?) {
        if let opening = value.lastIndex(of: "<"),
           let closing = value[opening...].firstIndex(of: ">"),
           let address = normalizedEmail(String(value[value.index(after: opening) ..< closing])) {
            let name = String(value[..<opening]).trimmingCharacters(in: .whitespacesAndNewlines)
            return (name.isEmpty ? address : name, address)
        }

        if let address = normalizedEmail(value) {
            return (address, address)
        }
        return (value.isEmpty ? "Unknown sender" : value, nil)
    }

    private static func normalizedEmail(_ value: String) -> String? {
        let email = value.trimmingCharacters(in: .whitespacesAndNewlines)
        let parts = email.split(separator: "@", omittingEmptySubsequences: false)
        guard parts.count == 2, !parts[0].isEmpty, !parts[1].isEmpty,
              !email.contains(where: \.isWhitespace) else { return nil }
        return "\(parts[0])@\(parts[1].lowercased())"
    }

    private static func emailDomain(_ address: String) -> String? {
        address.split(separator: "@", maxSplits: 1).last.map(String.init)
    }

    private static func mismatchWarning(displayName: String, actualDomain: String?) -> String? {
        guard let actualDomain else { return nil }
        let claimedDomains = domainClaims(in: displayName)
        guard let claim = claimedDomains.first(where: { !relatedDomains($0, actualDomain) }) else {
            return nil
        }
        return "The sender name claims \(claim), but the From address uses \(actualDomain)."
    }

    private static func domainClaims(in value: String) -> [String] {
        let pattern = #"(?i)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}"#
        guard let expression = try? NSRegularExpression(pattern: pattern) else { return [] }
        let range = NSRange(value.startIndex..., in: value)
        return expression.matches(in: value, options: [], range: range).compactMap { match in
            Range(match.range, in: value).map { value[$0].lowercased() }
        }
    }
}

struct ExternalLinkDestination: Identifiable, Equatable {
    let url: URL
    let visibleText: String

    var id: String { url.absoluteString }
    var host: String { url.host ?? "Unknown destination" }

    var warning: String? {
        guard let scheme = url.scheme?.lowercased(), ["http", "https"].contains(scheme),
              let destinationHost = url.host?.lowercased() else {
            return "This is not a supported web destination."
        }
        if url.user != nil || url.password != nil {
            return "This link disguises its destination with account information in the address."
        }
        if destinationHost.contains("xn--") {
            return "This destination uses an internationalized domain encoding that may resemble another site."
        }
        if Self.isIPAddress(destinationHost) {
            return "This link goes directly to a numeric network address instead of a named domain."
        }
        if let visibleHost = Self.visibleHost(in: visibleText),
           !relatedDomains(visibleHost, destinationHost) {
            return "The visible address says \(visibleHost), but the link opens \(destinationHost)."
        }
        return nil
    }

    static func detected(in text: String) -> [Self] {
        guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else {
            return []
        }
        let range = NSRange(text.startIndex..., in: text)
        var seen = Set<String>()
        return detector.matches(in: text, options: [], range: range).compactMap { result in
            guard let url = result.url,
                  let scheme = url.scheme?.lowercased(), ["http", "https"].contains(scheme),
                  seen.insert(url.absoluteString).inserted else { return nil }
            let visibleText = Range(result.range, in: text).map { String(text[$0]) } ?? url.absoluteString
            return Self(url: url, visibleText: visibleText)
        }
    }

    private static func visibleHost(in text: String) -> String? {
        let candidate = text.contains("://") ? text : "https://\(text)"
        return URL(string: candidate)?.host?.lowercased()
    }

    private static func isIPAddress(_ host: String) -> Bool {
        if host.contains(":") {
            return true
        }
        let octets = host.split(separator: ".", omittingEmptySubsequences: false)
        return octets.count == 4 && octets.allSatisfy { UInt8($0) != nil }
    }
}

private func relatedDomains(_ lhs: String, _ rhs: String) -> Bool {
    lhs == rhs || lhs.hasSuffix(".\(rhs)") || rhs.hasSuffix(".\(lhs)")
}
