import XCTest
import SwiftTreeSitter
import TreeSitterQsharp

final class TreeSitterQsharpTests: XCTestCase {
    func testCanLoadGrammar() throws {
        let parser = Parser()
        let language = Language(language: tree_sitter_qsharp())
        XCTAssertNoThrow(try parser.setLanguage(language),
                         "Error loading Qsharp grammar")
    }
}
