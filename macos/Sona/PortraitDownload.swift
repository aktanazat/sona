import Foundation

// URLSession delegates and task cancellation share only the state protected by lock.
final class PortraitDownload: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Data, Error>?
    private var session: URLSession?
    private var task: URLSessionDataTask?
    private var bytes = Data()
    private var finished = false
    private let url: URL

    init(url: URL) { self.url = url }

    static func publicURL(_ text: String) throws -> URL {
        guard let url = URL(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              url.scheme?.lowercased() == "https", let host = url.host?.lowercased(),
              url.user == nil, url.password == nil, url.port == nil || url.port == 443,
              host.contains("."), !host.hasSuffix(".local"), !host.hasSuffix(".localhost"),
              !host.hasSuffix(".internal"), !host.hasSuffix(".test"),
              host != "localhost", !host.contains(":"),
              host.unicodeScalars.contains(where: CharacterSet.letters.contains),
              host.unicodeScalars.allSatisfy({ CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyz0123456789-.").contains($0) }) else {
            throw PersonFailure(message: "Use a public HTTPS image address without a login.")
        }
        return url
    }

    func fetch() async throws -> Data {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                lock.lock()
                guard !finished else {
                    lock.unlock()
                    continuation.resume(throwing: CancellationError())
                    return
                }
                self.continuation = continuation
                let configuration = URLSessionConfiguration.ephemeral
                configuration.httpCookieStorage = nil
                configuration.urlCredentialStorage = nil
                configuration.httpShouldSetCookies = false
                configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
                configuration.timeoutIntervalForRequest = 20
                configuration.timeoutIntervalForResource = 30
                let queue = OperationQueue()
                queue.maxConcurrentOperationCount = 1
                let session = URLSession(configuration: configuration, delegate: self, delegateQueue: queue)
                var request = URLRequest(url: url)
                request.setValue("image/png,image/jpeg", forHTTPHeaderField: "Accept")
                request.setValue("Sona Portrait Import", forHTTPHeaderField: "User-Agent")
                let task = session.dataTask(with: request)
                self.session = session
                self.task = task
                lock.unlock()
                task.resume()
            }
        } onCancel: {
            self.finish(.failure(CancellationError()))
        }
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        guard let response = response as? HTTPURLResponse, response.statusCode == 200,
              ["image/png", "image/jpeg"].contains(response.mimeType?.lowercased() ?? ""),
              response.expectedContentLength <= PortraitImage.maximumSourceBytes else {
            completionHandler(.cancel)
            finish(.failure(PersonFailure(message: "Couldn't load that photo. Use a direct PNG or JPEG address under 10 MB.")))
            return
        }
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        lock.lock()
        guard !finished else { lock.unlock(); return }
        guard bytes.count + data.count <= PortraitImage.maximumSourceBytes else {
            lock.unlock()
            finish(.failure(PersonFailure(message: "That photo is larger than 10 MB.")))
            return
        }
        bytes.append(data)
        lock.unlock()
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        guard let destination = request.url,
              (try? Self.publicURL(destination.absoluteString)) != nil,
              destination.host?.lowercased() == url.host?.lowercased() else {
            completionHandler(nil)
            finish(.failure(PersonFailure(message: "This address redirects to another site. Paste the final image address.")))
            return
        }
        completionHandler(request)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if let error { finish(.failure(error)); return }
        lock.lock()
        let result = bytes
        lock.unlock()
        finish(.success(result))
    }

    private func finish(_ result: Result<Data, Error>) {
        lock.lock()
        guard !finished else { lock.unlock(); return }
        finished = true
        let continuation = self.continuation
        let task = self.task
        let session = self.session
        self.continuation = nil
        self.task = nil
        self.session = nil
        bytes = Data()
        lock.unlock()
        task?.cancel()
        session?.invalidateAndCancel()
        continuation?.resume(with: result)
    }
}
