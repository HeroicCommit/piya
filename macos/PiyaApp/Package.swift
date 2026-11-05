// swift-tools-version: 5.10
import PackageDescription

let package = Package(
    name: "PiyaApp",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "Piya", targets: ["PiyaApp"]),
    ],
    targets: [
        .executableTarget(
            name: "PiyaApp",
            dependencies: ["PiyaBindings"],
            path: "Sources/PiyaApp",
            resources: [.process("Resources")]
        ),
        .target(
            name: "PiyaBindings",
            dependencies: ["piyaFFI"],
            path: "Generated",
            sources: ["piya.swift"]
        ),
        .systemLibrary(
            name: "piyaFFI",
            path: "Generated"
        ),
    ]
)
