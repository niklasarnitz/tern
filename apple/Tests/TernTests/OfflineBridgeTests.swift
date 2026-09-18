import Foundation
@testable import Tern
import XCTest

final class OfflineBridgeTests: XCTestCase {
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
            XCTAssertEqual(details.recipients, ["Offline reader <reader@example.invalid>"])
            XCTAssertEqual(details.replyTo, ["Tern replies <reply@example.invalid>"])
            XCTAssertEqual(details.attachments.first?.filename, "quarterly-report.pdf")
            XCTAssertEqual(details.attachments.first?.size, 24_576)
            mailboxID = mailbox.id
        }

        let reopened = try MailClient(databasePath: path)
        let lastPage = try reopened.listMessages(mailboxId: mailboxID, offset: 100, limit: 100)
        XCTAssertEqual(lastPage.count, 5)
        XCTAssertEqual(lastPage.last?.remoteUid, 1)
        let beyondEnd = try reopened.listMessages(mailboxId: mailboxID, offset: 105, limit: 100)
        XCTAssertTrue(beyondEnd.isEmpty)
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
}
