// swift-tools-version: 5.10
// TamaleKit: everything the iOS app needs that isn't UI. The key, the hub's REST API,
// the end-to-end encrypted session channel and the /web messages. See apple/ios.md.

import PackageDescription

let package = Package(
    name: "TamaleKit",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "TamaleKit", targets: ["TamaleKit"]),
    ],
    targets: [
        .target(name: "TamaleKit"),
        // The tests read apple/Fixtures, which Rust tests write (see src/web/fixtures.rs).
        .testTarget(name: "TamaleKitTests", dependencies: ["TamaleKit"]),
    ]
)
