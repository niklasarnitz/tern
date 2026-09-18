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
