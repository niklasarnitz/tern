import Foundation
@testable import Tern
import XCTest

final class OfflineBridgeTests: XCTestCase {
    func testSenderIdentityExposesAddressAndDetectsClaimedDomainMismatch() {
        let suspicious = SenderIdentity("accounts.example.com <security@lookalike.test>")
        XCTAssertEqual(suspicious.displayName, "accounts.example.com")
        XCTAssertEqual(suspicious.address, "security@lookalike.test")
        XCTAssertEqual(suspicious.domain, "lookalike.test")
        XCTAssertNotNil(suspicious.mismatchWarning)

        let legitimate = SenderIdentity("Example <notices@mail.example.com>")
        XCTAssertEqual(legitimate.address, "notices@mail.example.com")
        XCTAssertNil(legitimate.mismatchWarning)
    }

    func testExternalLinksExposeDestinationAndFlagVisibleHostMismatch() throws {
        let destination = ExternalLinkDestination(
            url: try XCTUnwrap(URL(string: "https://login.lookalike.test/session")),
            visibleText: "https://accounts.example.com"
        )
        XCTAssertEqual(destination.host, "login.lookalike.test")
        XCTAssertNotNil(destination.warning)

        let detected = ExternalLinkDestination.detected(
            in: "Review at https://safe.example.test/account before Friday."
        )
        XCTAssertEqual(detected.map(\.url.absoluteString), ["https://safe.example.test/account"])
        XCTAssertNil(detected.first?.warning)
    }

    func testReopensRustFixtureAndReadsBoundedPagesWithoutCredentials() throws {
        let path = try XCTUnwrap(ProcessInfo.processInfo.environment["TERN_TEST_DATABASE"])
        let mailboxID: String
        do {
            let firstClient = try MailClient(databasePath: path)
            let accounts = try firstClient.listAccounts()
            XCTAssertEqual(accounts.count, 1)
            XCTAssertEqual(accounts.first?.imapHost, "unreachable.invalid")
            let mailboxes = try firstClient.listMailboxes(accountId: "fixture")
            let mailbox = try XCTUnwrap(mailboxes.first)
            let firstPage = try firstClient.listMessages(mailboxId: mailbox.id, offset: 0, limit: 100)
            XCTAssertEqual(firstPage.count, 100)
            XCTAssertEqual(firstPage.first?.remoteUid, 105)
            XCTAssertEqual(firstPage.first?.subject, "Offline message 105")
            XCTAssertEqual(firstPage.first?.isStarred, true)
            let messageID = try XCTUnwrap(firstPage.first?.id)
            let details = try XCTUnwrap(try firstClient.messageDetails(messageId: messageID))
            XCTAssertEqual(details.recipients, ["Reader <reader@example.invalid>"])
            XCTAssertEqual(details.replyTo, ["Tern replies <reply@example.invalid>"])
            XCTAssertEqual(details.attachments.first?.filename, "quarterly-report.pdf")
            XCTAssertEqual(details.attachments.first?.size, 24_576)
            let search = try firstClient.searchMessages(
                query: "subject:\"Offline message 95\" to:reader@example.invalid is:starred",
                offset: 0,
                limit: 10
            )
            XCTAssertEqual(search.map(\.remoteUid), [95])
            mailboxID = mailbox.id
        }

        let reopened = try MailClient(databasePath: path)
        let lastPage = try reopened.listMessages(mailboxId: mailboxID, offset: 100, limit: 100)
        XCTAssertEqual(lastPage.count, 5)
        XCTAssertEqual(lastPage.last?.remoteUid, 1)
        let beyondEnd = try reopened.listMessages(mailboxId: mailboxID, offset: 105, limit: 100)
        XCTAssertTrue(beyondEnd.isEmpty)
    }

    func testWidgetSnapshotIsBoundedToExplicitMailboxSelection() throws {
        let path = try XCTUnwrap(ProcessInfo.processInfo.environment["TERN_TEST_DATABASE"])
        let client = try MailClient(databasePath: path)
        let mailbox = try XCTUnwrap(try client.listMailboxes(accountId: "fixture").first)

        let empty = try client.widgetSnapshot(mailboxIds: [], importantLimit: 20)
        XCTAssertEqual(empty.unreadCount, 0)
        XCTAssertTrue(empty.mailboxes.isEmpty)
        XCTAssertTrue(empty.importantMessages.isEmpty)

        let selected = try client.widgetSnapshot(mailboxIds: [mailbox.id], importantLimit: 20)
        XCTAssertEqual(selected.unreadCount, 53)
        XCTAssertEqual(selected.mailboxes.first?.id, mailbox.id)
        XCTAssertLessThanOrEqual(selected.importantMessages.count, 10)
    }

    func testWidgetPrivacyRemovesUnapprovedMessageContent() throws {
        let defaults = WidgetDataStore.defaults
        let previous = defaults.data(forKey: WidgetDataStore.snapshotKey)
        defer { defaults.set(previous, forKey: WidgetDataStore.snapshotKey) }
        let snapshot = WidgetSnapshot(
            unreadCount: 4,
            mailboxes: [WidgetMailboxSummary(id: "inbox", displayName: "Inbox", unreadCount: 4)],
            importantMessages: [
                MessageSummary(
                    id: "message",
                    mailboxId: "inbox",
                    remoteUid: 7,
                    subject: "Private subject",
                    sender: "Private sender",
                    date: "2026-09-18",
                    snippet: "Private preview",
                    isRead: false,
                    isStarred: true,
                    hasAttachments: false
                ),
            ]
        )

        try WidgetDataStore.cache(snapshot, privacy: .countsOnly)
        XCTAssertTrue(WidgetDataStore.snapshot().importantMessages.isEmpty)

        try WidgetDataStore.cache(snapshot, privacy: .senderAndSubject)
        XCTAssertEqual(WidgetDataStore.snapshot().importantMessages.first?.subject, "Private subject")
        XCTAssertNil(WidgetDataStore.snapshot().importantMessages.first?.snippet)

        try WidgetDataStore.cache(snapshot, privacy: .fullPreview)
        XCTAssertEqual(WidgetDataStore.snapshot().importantMessages.first?.snippet, "Private preview")
    }

    @MainActor
    func testNativeStorePagesWithoutAccumulatingMessages() async {
        let store = MailStore()
        await store.start()
        XCTAssertNil(store.errorMessage)
        XCTAssertEqual(store.messages.count, 100)
        XCTAssertTrue(store.hasNextMessagePage)
        store.selectedMessageID = store.messages.first?.id
        await store.loadSelectedMessageDetails()
        XCTAssertEqual(store.selectedMessageDetails?.listId, ["Tern Updates <updates.tern.example>"])
        await store.nextMessagePage()
        XCTAssertEqual(store.messages.count, 5)
        XCTAssertEqual(store.messageOffset, 100)
        XCTAssertFalse(store.hasNextMessagePage)
        await store.previousMessagePage()
        XCTAssertEqual(store.messages.count, 100)
        XCTAssertEqual(store.messageOffset, 0)
        await store.refresh()
        XCTAssertNil(store.errorMessage)
        XCTAssertEqual(store.accounts.count, 1)
        XCTAssertEqual(store.messages.count, 100)
    }

    @MainActor
    func testNativeStoreSearchesAndRestoresTheMailboxPage() async {
        let store = MailStore()
        await store.start()
        store.searchText = "subject:\"Offline message 95\" is:starred"
        await store.submitSearch()
        XCTAssertNil(store.errorMessage)
        XCTAssertTrue(store.isSearching)
        XCTAssertEqual(store.messages.map(\.remoteUid), [95])

        store.searchText = ""
        await store.clearSearch()
        XCTAssertFalse(store.isSearching)
        XCTAssertEqual(store.messages.count, 100)
    }
}
