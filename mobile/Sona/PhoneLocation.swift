import CoreLocation

/// One reading of where the phone is, taken while Sona is open. The first
/// read asks for permission; nothing reads in the background.
@MainActor
final class LocationOnce: NSObject, @preconcurrency CLLocationManagerDelegate {
    private let manager = CLLocationManager()
    private var waiting: [CheckedContinuation<CLLocation?, Never>] = []

    override init() {
        super.init()
        manager.delegate = self
    }

    /// `nil` when he declined, Location Services are off, or no fix came.
    func read() async -> CLLocation? {
        await withCheckedContinuation { continuation in
            waiting.append(continuation)
            if waiting.count == 1 { advance() }
        }
    }

    private func advance() {
        switch manager.authorizationStatus {
        case .notDetermined: manager.requestWhenInUseAuthorization()
        case .authorizedAlways, .authorizedWhenInUse: manager.requestLocation()
        default: finish(nil)
        }
    }

    func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        guard !waiting.isEmpty, manager.authorizationStatus != .notDetermined else { return }
        advance()
    }

    func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        finish(locations.last)
    }

    func locationManager(_ manager: CLLocationManager, didFailWithError error: any Error) {
        finish(nil)
    }

    private func finish(_ reading: CLLocation?) {
        let waiters = waiting
        waiting = []
        for waiter in waiters { waiter.resume(returning: reading) }
    }
}

extension DeviceRecordingManifest.PlaceReading {
    init(_ reading: CLLocation) {
        self.init(
            latitude: reading.coordinate.latitude,
            longitude: reading.coordinate.longitude,
            accuracy_meters: reading.horizontalAccuracy
        )
    }
}
