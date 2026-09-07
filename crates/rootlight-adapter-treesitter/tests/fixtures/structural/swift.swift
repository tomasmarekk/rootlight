// Swift declaration, scope and source-evidence fixture.
// External imports and dynamic dispatch are not evaluated.
import Foundation

/// Stored greeting 🌍.
protocol Store {
    func load(_ key: String) -> String
}
struct Entry {
    var value: String
    func render() -> String { return value }
}
class Cache {
    let size = 1
    init() {}
    deinit {}
}
actor Worker { func run() {} }
enum Result {
    case ready, missing
    case loaded(String)
}
extension Entry {
    func copy() -> Entry { return self }
}
typealias Label = String
func greet(_ name: String) -> String {
    print(name)
    return "Hello 🌍"
}
let title = "hello"
