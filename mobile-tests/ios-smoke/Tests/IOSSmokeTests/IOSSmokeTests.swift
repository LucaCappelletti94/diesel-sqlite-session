import XCTest
@testable import IOSSmoke

final class IOSSmokeTests: XCTestCase {
    func testReplicationRoundtrip() {
        XCTAssertEqual(runSmokeCase(.replicationRoundtrip), 0)
    }

    func testConflictAbort() {
        XCTAssertEqual(runSmokeCase(.conflictAbort), 0)
    }

    func testInvalidTableName() {
        XCTAssertEqual(runSmokeCase(.invalidTableName), 0)
    }

    func testConflictHandlerPanicMapsError() {
        XCTAssertEqual(runSmokeCase(.conflictHandlerPanicMapsError), 0)
    }
}
