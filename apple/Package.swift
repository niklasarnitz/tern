// swift-tools-version: 6.0
import Foundation
import PackageDescription

let packageDirectory = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()
    .path
let generatedDirectory = "\(packageDirectory)/Generated"
let rustDebugDirectory = "\(packageDirectory)/../core/target/debug"
let generatedModuleMapFlags = [
    "-Xcc",
    "-fmodule-map-file=\(generatedDirectory)/mail_coreFFI.modulemap",
    "-Xcc",
    "-fmodule-map-file=\(generatedDirectory)/mail_modelFFI.modulemap",
]

/// The UniFFI output is deliberately kept outside Sources/Tern. The Rust build
/// places mail_core.swift, mail_coreFFI.h, and mail_coreFFI.modulemap here.
let package = Package(
    name: "Tern",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(name: "Tern", targets: ["Tern"]),
    ],
    targets: [
        .executableTarget(
            name: "Tern",
            path: ".",
            exclude: [
                "README.md",
                "Generated/.gitkeep",
                "Generated/mail_coreFFI.h",
                "Generated/mail_coreFFI.modulemap",
                "Generated/mail_modelFFI.h",
                "Generated/mail_modelFFI.modulemap",
            ],
            sources: [
                "Sources/Tern",
                "Generated",
            ],
            swiftSettings: [
                .unsafeFlags(["-warnings-as-errors"]),
                // Generated Swift imports the C module emitted by UniFFI.
                .unsafeFlags(generatedModuleMapFlags + [
                    "-Xcc",
                    "-I\(generatedDirectory)",
                ]),
            ],
            linkerSettings: [
                // Pass the archive by absolute path so SwiftPM cannot select a
                // same-named dynamic library from the Rust target directory.
                .unsafeFlags(["\(rustDebugDirectory)/libmail_core.a"], .when(platforms: [.macOS])),
            ]
        ),
    ],
    swiftLanguageModes: [.v6]
)
