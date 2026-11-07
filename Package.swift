// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "CamCam",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "camcam", targets: ["CamCam"])],
    dependencies: [
        .package(url: "https://github.com/dduan/TOMLDecoder.git", exact: "0.4.5"),
    ],
    targets: [
        .systemLibrary(name: "CFFmpeg", pkgConfig: "libavformat", providers: [.brew(["ffmpeg"])]),
        .target(name: "CCamVideo", dependencies: ["CFFmpeg"], linkerSettings: [
            .linkedLibrary("avcodec"), .linkedLibrary("avutil"),
            .linkedFramework("CoreVideo"),
        ]),
        .executableTarget(name: "CamCam", dependencies: ["CCamVideo", "TOMLDecoder"]),
        .testTarget(name: "CamCamTests", dependencies: ["CamCam", "CCamVideo"],
                    exclude: ["Fixtures"]),
    ],
    swiftLanguageModes: [.v5]
)
