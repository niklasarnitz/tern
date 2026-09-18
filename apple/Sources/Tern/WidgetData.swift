import Foundation

#if canImport(WidgetKit)
    import WidgetKit
#endif
extension WidgetDataStore {
    static func cache(_ snapshot: WidgetSnapshot, privacy: WidgetPrivacy, now: Date = Date()) throws {
        let messages: [CachedWidgetMessage] = switch privacy {
        case .countsOnly:
            []
        case .senderAndSubject, .fullPreview:
            snapshot.importantMessages.map { message in
                CachedWidgetMessage(
                    id: message.id,
                    mailboxID: message.mailboxId,
                    sender: message.sender,
                    subject: message.subject,
                    date: message.date,
                    snippet: privacy == .fullPreview ? message.snippet : nil
                )
            }
        }
        let cached = CachedWidgetSnapshot(
            updatedAt: now,
            unreadCount: snapshot.unreadCount,
            mailboxes: snapshot.mailboxes.map { mailbox in
                CachedWidgetMailbox(
                    id: mailbox.id,
                    displayName: mailbox.displayName,
                    unreadCount: mailbox.unreadCount
                )
            },
            importantMessages: messages
        )
        try defaults.set(JSONEncoder().encode(cached), forKey: snapshotKey)
        #if canImport(WidgetKit)
            WidgetCenter.shared.reloadAllTimelines()
        #endif
    }
}
