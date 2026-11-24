// Prints the words from standard input, one per line, that macOS's spell checker accepts in
// the language given as the first argument (such as en_US), in the order they came.
// Used by build-next-words.ts so the English common words hold no misspellings.

import AppKit

let language = CommandLine.arguments[1]
let checker = NSSpellChecker.shared
while let word = readLine() {
    let range = checker.checkSpelling(
        of: word, startingAt: 0, language: language, wrap: false, inSpellDocumentWithTag: 0, wordCount: nil
    )
    if range.location == NSNotFound { print(word) }
}
