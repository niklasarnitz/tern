import Foundation

enum WidgetPrivacy: String, CaseIterable, Codable, Identifiable {
    case countsOnly
    case senderAndSubject
    case fullPreview

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .countsOnly:
            "Counts only"
        case .senderAndSubject:
            "Senders and subjects"
        case .fullPreview:
            "Message previews"
        }
    }
}

struct CachedWidgetMessage: Codable, Equatable {
    let id: String
    let mailboxID: String
    let sender: String
    let subject: String
    let date: String
    let snippet: String?
}

struct CachedWidgetMailbox: Codable, Equatable {
    let id: String
    let displayName: String
    let unreadCount: UInt32
}

struct CachedWidgetSnapshot: Codable, Equatable {
    let updatedAt: Date
    let unreadCount: UInt32
    let mailboxes: [CachedWidgetMailbox]
    let importantMessages: [CachedWidgetMessage]

    static let empty = Self(
        updatedAt: .distantPast,
        unreadCount: 0,
        mailboxes: [],
        importantMessages: []
    )
}

enum WidgetDataStore {
    static let appGroup = "group.com.niklasarnitz.tern"
    static let selectedMailboxIDsKey = "widgets.selectedMailboxIDs"
    static let privacyKey = "widgets.privacy"
    static let snapshotKey = "widgets.snapshot"

    static var defaults: UserDefaults {
        UserDefaults(suiteName: appGroup) ?? .standard
    }

    static func selectedMailboxIDs() -> Set<String> {
        Set(defaults.stringArray(forKey: selectedMailboxIDsKey) ?? [])
    }

    static func setSelectedMailboxIDs(_ ids: Set<String>) {
        defaults.set(ids.sorted(), forKey: selectedMailboxIDsKey)
    }

    static func privacy() -> WidgetPrivacy {
        guard let value = defaults.string(forKey: privacyKey),
              let privacy = WidgetPrivacy(rawValue: value) else { return .countsOnly }
        return privacy
    }

    static func setPrivacy(_ privacy: WidgetPrivacy) {
        defaults.set(privacy.rawValue, forKey: privacyKey)
    }

    static func snapshot() -> CachedWidgetSnapshot {
        guard let data = defaults.data(forKey: snapshotKey),
              let snapshot = try? JSONDecoder().decode(CachedWidgetSnapshot.self, from: data)
        else {
            return .empty
        }
        return snapshot
    }
}
