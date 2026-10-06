import SwiftUI

/// The example app over `junk-ffi`: find a ring, sync it, run a workout on it.
///
/// Three screens over the three functions the Rust exports, and nothing else. There is no
/// Bluetooth here and no protocol: every frame, channel and byte stays on the Rust side
/// (SPEC §4), so what this app knows about a ring is `FoundRing`, `Progress`, `SyncResult`
/// and `LiveResult` — numbers and strings.
@main
struct JunkIOSApp: App {
    @State private var model = RingModel()

    var body: some Scene {
        WindowGroup {
            TabView {
                ScanView(model: model)
                    .tabItem { Label("Scan", systemImage: "dot.radiowaves.left.and.right") }
                SyncView(model: model)
                    .tabItem { Label("Sync", systemImage: "arrow.triangle.2.circlepath") }
                LiveView(model: model)
                    .tabItem { Label("Live", systemImage: "heart.fill") }
            }
        }
    }
}
