import SwiftUI

/// The rings in range, and which of them the other two screens talk to.
struct ScanView: View {
    let model: RingModel

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Button {
                        Task { await model.findRings() }
                    } label: {
                        HStack {
                            Text(model.scanning ? "Scanning…" : "Scan")
                            if model.scanning {
                                Spacer()
                                ProgressView()
                            }
                        }
                    }
                    .disabled(model.scanning)
                } footer: {
                    Text("Listens for \(RingModel.timeoutSecs) seconds. Rings only: a ring is what advertises the service the driver knows.")
                }

                if let trouble = model.scanTrouble {
                    Section("No luck") {
                        Text(trouble).font(.callout)
                    }
                }

                Section("Rings") {
                    if model.rings.isEmpty {
                        Text(model.scanTrouble == nil ? "Nothing yet." : "None.")
                            .foregroundStyle(.secondary)
                    }
                    ForEach(model.rings, id: \.id) { ring in
                        Button {
                            model.selectedID = ring.id
                        } label: {
                            row(for: ring)
                        }
                        .buttonStyle(.plain)
                    }
                }

                Section("Talking to") {
                    Text(model.selectedID ?? "whichever single ring is in range")
                        .font(.system(.footnote, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Scan")
        }
    }

    private func row(for ring: FoundRing) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(ring.name ?? "(no name)")
                Text(ring.id)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Text(ring.rssi.map { "\($0) dBm" } ?? "—")
                .font(.caption)
                .foregroundStyle(.secondary)
            if ring.id == model.selectedID {
                Image(systemName: "checkmark").foregroundStyle(.tint)
            }
        }
        .contentShape(Rectangle())
    }
}
