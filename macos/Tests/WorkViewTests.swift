import XCTest

/// Saved views as the core sends them, read the way the app's core client
/// reads every answer.
final class WorkViewTests: XCTestCase {
    private func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }

    /// A view saved from the command line or by an agent with "this or that"
    /// once stopped the whole saved views list from loading.
    func testAViewWithAnAnyGroupLoadsWithItsGroups() throws {
        let filters = """
        [{"field": "status", "op": "is_not", "values": ["done"]},
         {"any": [
           {"field": "priority", "op": "is", "values": ["1"]},
           {"all": [{"field": "due", "op": "before", "values": ["today"]},
                    {"field": "owner", "op": "is", "values": ["me"]}]}
         ]}]
        """
        let json = """
        [{"id": "v1", "name": "Urgent or overdue", "revision": 2, "icon": "flame", "projectId": null,
          "query": {"includeArchived": false, "includeSnoozed": false, "filters": \(filters)},
          "display": {"layout": "list", "groupBy": "status", "orderBy": "priority", "showEmptyGroups": false,
                      "properties": [], "showSubTasks": true, "subGroupBy": "none", "hiddenColumns": [],
                      "orderDirection": "ascending", "noPriorityFirst": true, "nestSubTasks": false}}]
        """
        let views = try decoder().decode([WorkView].self, from: Data(json.utf8))
        let read = try JSONEncoder().encode(XCTUnwrap(views.first?.query.filters))
        XCTAssertEqual(try JSONSerialization.jsonObject(with: read) as? NSArray,
                       try JSONSerialization.jsonObject(with: Data(filters.utf8)) as? NSArray)
        XCTAssertEqual(views.first?.icon, "flame")
    }

    /// The app saves a view's groups back in the shape the core reads.
    func testGroupsSaveInTheShapeTheyLoadIn() throws {
        let filters: [WorkFilter] = [.any([
            .condition(WorkCondition(field: .label, op: .includesAny, values: ["l1"])),
            .all([.condition(WorkCondition(field: .project, op: .is, values: ["none"]))]),
        ])]
        let data = try JSONEncoder().encode(filters)
        let sent = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [[String: Any]])
        let any = try XCTUnwrap(sent.first?["any"] as? [[String: Any]])
        XCTAssertEqual(any.first?["field"] as? String, "label")
        XCTAssertNotNil(any.last?["all"])
        XCTAssertEqual(try JSONDecoder().decode([WorkFilter].self, from: data), filters)
    }
}
