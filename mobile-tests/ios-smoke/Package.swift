// swift-tools-version: 6.0
import Foundation
import PackageDescription

let rustLibDir = ProcessInfo.processInfo.environment["RUST_SMOKE_LIB_DIR"] ?? ""
var smokeLinkerSettings: [LinkerSetting] = [.linkedLibrary("c++")]
if !rustLibDir.isEmpty {
    smokeLinkerSettings.append(.unsafeFlags(["-L", rustLibDir, "-ldiesel_sqlite_session"]))
}

let package = Package(
    name: "IOSSmoke",
    platforms: [.iOS(.v17)],
    products: [
        .library(name: "IOSSmoke", targets: ["IOSSmoke"]),
    ],
    targets: [
        .target(
            name: "IOSSmoke",
            path: "Sources/IOSSmoke",
            linkerSettings: smokeLinkerSettings
        ),
        .testTarget(
            name: "IOSSmokeTests",
            dependencies: ["IOSSmoke"],
            path: "Tests/IOSSmokeTests"
        ),
    ]
)
