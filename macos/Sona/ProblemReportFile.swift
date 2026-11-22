import Darwin
import Foundation

struct ProblemReportContext: Sendable {
    let historyID: Int64?
    let version: String?
    let build: String?
    let operatingSystem: String
    let created: Date

    init(historyID: Int64?) {
        self.historyID = historyID
        version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String
        operatingSystem = ProcessInfo.processInfo.operatingSystemVersionString
        created = .now
    }

    var heading: String {
        var heading = "Sona problem report\nCreated: \(created.ISO8601Format())\nmacOS: \(operatingSystem)"
        if let version { heading += "\nApp version: \(version)" }
        if let build { heading += "\nApp build: \(build)" }
        if let historyID { heading += "\nDictation ID: \(historyID)" }
        return heading
    }
}

enum ProblemReportFile {
    static let maximumLogBytes = 500 * 1024

    static func save(
        context: ProblemReportContext, description: String, logDirectory: String?
    ) async throws -> URL {
        try Task.checkCancellation()
        let worker = Task.detached(priority: .utility) {
            try write(context: context, description: description, logDirectory: logDirectory)
        }
        return try await withTaskCancellationHandler {
            try await worker.value
        } onCancel: {
            worker.cancel()
        }
    }

    private static func write(
        context: ProblemReportContext, description: String, logDirectory: String?
    ) throws -> URL {
        try Task.checkCancellation()
        let log = try logDirectory.map(readLog)
        let directory = try FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true
        ).appending(path: "Sona/Problem Reports", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(
            at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let destination = directory.appending(path: "Sona Problem Report \(UUID().uuidString).txt")
        var template = directory.appending(path: ".sona-report-XXXXXX").path.utf8CString
        try template.withUnsafeMutableBufferPointer { buffer in
            guard let temporaryPath = buffer.baseAddress else { throw POSIXError(.EINVAL) }
            let descriptor = mkstemp(temporaryPath)
            guard descriptor >= 0 else { throw systemError() }
            let output = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
            defer { _ = unlink(temporaryPath) }
            guard fcntl(descriptor, F_SETFD, FD_CLOEXEC) == 0 else {
                throw systemError()
            }
            let heading = "\(context.heading)\n\nWhat went wrong\n\(description)\n"
            try output.write(contentsOf: Data(heading.utf8))
            if let log {
                let note = log.truncated ? "\nRecent logs (newest 500 KiB; may start mid-line)\n" : "\nRecent logs\n"
                try output.write(contentsOf: Data(note.utf8))
                try output.write(contentsOf: log.data)
            }
            try output.synchronize()
            try Task.checkCancellation()
            guard rename(temporaryPath, destination.path) == 0 else {
                throw systemError()
            }
        }
        return destination
    }

    private static func systemError() -> NSError {
        NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
    }

    private static func readLog(directory: String) throws -> (data: Data, truncated: Bool) {
        let url = URL(fileURLWithPath: directory).appending(path: LogTailReader.logFileName)
        let descriptor = open(url.path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        guard descriptor >= 0 else { throw systemError() }
        let input = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
        var info = stat()
        guard fstat(descriptor, &info) == 0 else {
            throw systemError()
        }
        guard info.st_mode & S_IFMT == S_IFREG else { throw POSIXError(.EINVAL) }
        let truncated = info.st_size > maximumLogBytes
        if truncated { try input.seek(toOffset: UInt64(info.st_size - Int64(maximumLogBytes))) }
        let data = try input.read(upToCount: maximumLogBytes) ?? Data()
        return (data, truncated)
    }
}
