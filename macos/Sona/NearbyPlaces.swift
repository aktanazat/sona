import CoreLocation
import MapKit

/// One reading of where this Mac is, taken while Sona is in use. The first
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

    /// True until he answers the permission prompt the first read raises.
    var asks: Bool { manager.authorizationStatus == .notDetermined }

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

/// Places a meeting is held "at" as a whole. A reading in a hotel's lobby is
/// nearer the café there than the hotel's own pin, so one is always offered.
private let meetingVenues: [MKPointOfInterestCategory] = [
    .hotel, .airport, .conventionCenter, .museum, .stadium, .theater, .musicVenue,
]
private let meetingSpots: [MKPointOfInterestCategory] = [
    .restaurant, .cafe, .bakery, .brewery, .winery, .distillery, .nightlife, .park,
]

/// Up to three places a meeting may have been held around `reading`, nearest
/// first, with the nearest venue among them. None for a reading too rough to
/// tell one block from the next. Maps also lists the offices, banks, shops and
/// schools there, often under made-up names, so only these kinds are asked
/// for.
func placesNear(_ reading: CLLocation) async -> [ConversationLocation] {
    guard (0...500).contains(reading.horizontalAccuracy) else { return [] }
    let request = MKLocalPointsOfInterestRequest(
        center: reading.coordinate, radius: reading.horizontalAccuracy + 100)
    request.pointOfInterestFilter = MKPointOfInterestFilter(including: meetingVenues + meetingSpots)
    guard let response = try? await MKLocalSearch(request: request).start() else { return [] }
    var named = Set<String>()
    let nearest = response.mapItems
        .compactMap { item -> (place: ConversationLocation, venue: Bool, meters: CLLocationDistance)? in
            guard let name = item.name else { return nil }
            let coordinate: CLLocationCoordinate2D
            if #available(macOS 26, *) {
                coordinate = item.location.coordinate
            } else {
                coordinate = item.placemark.coordinate
            }
            let at = CLLocation(latitude: coordinate.latitude, longitude: coordinate.longitude)
            return (ConversationLocation(name: name, latitude: coordinate.latitude, longitude: coordinate.longitude),
                    item.pointOfInterestCategory.map(meetingVenues.contains) ?? false,
                    reading.distance(from: at))
        }
        .sorted { $0.meters < $1.meters }
        .filter { named.insert($0.place.name).inserted }
    let offered = nearest.prefix(3)
    guard let venue = nearest.first(where: \.venue), !offered.contains(where: \.venue) else {
        return offered.map(\.place)
    }
    return (offered.prefix(2) + [venue]).map(\.place)
}
