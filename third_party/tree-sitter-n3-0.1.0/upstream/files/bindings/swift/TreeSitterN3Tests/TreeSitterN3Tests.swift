import XCTest
import SwiftTreeSitter
import TreeSitterN3

final class TreeSitterN3Tests: XCTestCase {
    func testCanLoadGrammar() throws {
        let parser = Parser()
        let language = Language(language: tree_sitter_n3())
        XCTAssertNoThrow(try parser.setLanguage(language),
                         "Error loading N3 grammar")
    }
}
