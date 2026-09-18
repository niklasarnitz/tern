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
            mailboxID = mailbox.id
        }

        let reopened = try MailClient(databasePath: path)
        let lastPage = try reopened.listMessages(mailboxId: mailboxID, offset: 100, limit: 100)
        XCTAssertEqual(lastPage.count, 5)
        XCTAssertEqual(lastPage.last?.remoteUid, 1)
        let beyondEnd = try reopened.listMessages(mailboxId: mailboxID, offset: 105, limit: 100)
        XCTAssertTrue(beyondEnd.isEmpty)
    }
}
