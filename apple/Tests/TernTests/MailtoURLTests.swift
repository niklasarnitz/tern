import Foundation
@testable import Tern
import XCTest

final class MailtoURLTests: XCTestCase {
    func testParsesRecipientsHeadersUnicodeAndBodyWithoutTreatingPlusAsSpace() throws {
        let urlString = [
            "mailto:first@example.com,second@example.com",
            "?to=third@example.com&cc=copy1@example.com,copy2@example.com",
            "&BCC=blind@example.com&subject=Status%20%26%20caf%C3%A9",
            "&body=First+line%0D%0ASecond%20line",
        ].joined()
        let url = try XCTUnwrap(URL(string: urlString))

        let draft = try XCTUnwrap(MailtoURLParser.parse(url))

        XCTAssertEqual(draft.recipients, ["first@example.com", "second@example.com", "third@example.com"])
        XCTAssertEqual(draft.carbonCopyRecipients, ["copy1@example.com", "copy2@example.com"])
        XCTAssertEqual(draft.blindCarbonCopyRecipients, ["blind@example.com"])
        XCTAssertEqual(draft.subject, "Status & café")
        XCTAssertEqual(draft.body, "First+line\r\nSecond line")
    }

    func testEncodedCommaStaysInsideOneRecipient() throws {
        let url = try XCTUnwrap(URL(string: "mailto:quoted%2Cname@example.com?cc=other%2Cname@example.com"))

        let draft = try XCTUnwrap(MailtoURLParser.parse(url))

        XCTAssertEqual(draft.recipients, ["quoted,name@example.com"])
        XCTAssertEqual(draft.carbonCopyRecipients, ["other,name@example.com"])
    }

    func testParsesOnlyLocalFileAttachments() throws {
        let urlString = [
            "mailto:user@example.com",
            "?attach=file%3A%2F%2F%2FUsers%2Fme%2FQuarterly%2520Report.pdf",
            "&attachment=%2FUsers%2Fme%2Fnotes.txt",
            "&attach=https%3A%2F%2Fexample.com%2Ftracking.gif",
            "&attach=file%3A%2F%2Fserver.example.com%2Fshared%2Fsecret.txt",
        ].joined()
        let url = try XCTUnwrap(URL(string: urlString))

        let draft = try XCTUnwrap(MailtoURLParser.parse(url))

        XCTAssertEqual(draft.attachments.map(\.path), [
            "/Users/me/Quarterly Report.pdf",
            "/Users/me/notes.txt",
        ])
    }

    func testIgnoresUnsupportedHeadersAndKeepsFirstSubjectAndBody() throws {
        let urlString = [
            "mailto:user@example.com",
            "?subject=First&subject=Second&body=Original",
            "&body=Replacement&from=attacker@example.com",
        ].joined()
        let url = try XCTUnwrap(URL(string: urlString))

        let draft = try XCTUnwrap(MailtoURLParser.parse(url))

        XCTAssertEqual(draft.subject, "First")
        XCTAssertEqual(draft.body, "Original")
        XCTAssertEqual(draft.recipients, ["user@example.com"])
    }

    func testRejectsOtherSchemes() throws {
        let url = try XCTUnwrap(URL(string: "https://example.com/?subject=Not%20mail"))

        XCTAssertNil(MailtoURLParser.parse(url))
    }
}
