import AppKit

/// A coding agent Sona can start on a delegated task, in the terminal macOS opens
/// `.command` files with. The agent's Sona hook reports the session, so its progress
/// shows on the task as it does for a session started by hand.
enum AgentLaunch: String, CaseIterable {
    case claude = "Claude Code"
    case codex = "Codex"

    var name: String { rawValue }

    private var command: String {
        switch self {
        case .claude: "claude"
        case .codex: "codex"
        }
    }

    enum Failure: LocalizedError {
        case cannotOpen

        var errorDescription: String? { "The terminal could not be opened." }
    }

    /// The folder the agent works in for a project's tasks. The picker opens at the folder
    /// used last for that project, so Return keeps it; nil when the pick is cancelled.
    @MainActor
    static func folder(for projectId: String?) -> URL? {
        let key = "agentFolder." + (projectId ?? "none")
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Choose the folder the agent works in."
        panel.prompt = "Start"
        if let kept = UserDefaults.standard.string(forKey: key) {
            panel.directoryURL = URL(fileURLWithPath: kept, isDirectory: true)
        }
        guard panel.runModal() == .OK, let url = panel.url else { return nil }
        UserDefaults.standard.set(url.path, forKey: key)
        return url
    }

    /// Writes the prompt and a script that runs the agent on it from `folder`, then opens
    /// the script. The script removes both files before the agent starts. The prompt goes
    /// through a file so no character in it reaches the shell as syntax.
    func open(prompt: String, in folder: URL) throws {
        let files = FileManager.default
        let directory = files.temporaryDirectory.appending(path: "sona-agent-start", directoryHint: .isDirectory)
        try files.createDirectory(at: directory, withIntermediateDirectories: true)
        let id = UUID().uuidString
        let promptFile = directory.appending(path: id + ".txt")
        let script = directory.appending(path: id + ".command")
        try prompt.write(to: promptFile, atomically: true, encoding: .utf8)
        let text = """
            #!/bin/zsh -l
            cd -- \(Self.quoted(folder.path)) || exit 1
            prompt="$(cat -- \(Self.quoted(promptFile.path)))"
            rm -f -- \(Self.quoted(promptFile.path)) \(Self.quoted(script.path))
            exec \(command) "$prompt"

            """
        try text.write(to: script, atomically: true, encoding: .utf8)
        try files.setAttributes([.posixPermissions: 0o700], ofItemAtPath: script.path)
        guard NSWorkspace.shared.open(script) else {
            try? files.removeItem(at: promptFile)
            try? files.removeItem(at: script)
            throw Failure.cannotOpen
        }
    }

    private static func quoted(_ text: String) -> String {
        "'" + text.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }
}
