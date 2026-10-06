import SwiftUI

/// A session over the selected ring: its logs for the last few days, and what it is.
struct SyncView: View {
    @Bindable var model: RingModel

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Stepper("Days: \(model.days)", value: $model.days, in: 1...7)
                    Button {
                        Task { await model.syncRing() }
                    } label: {
                        HStack {
                            Text(model.syncing ? "Syncing…" : "Sync")
                            if model.syncing {
                                Spacer()
                                ProgressView()
                            }
                        }
                    }
                    .disabled(model.syncing)
                    if model.syncing {
                        Button("Stop", role: .destructive) { model.stopSync() }
                    }
                } footer: {
                    Text(
                        "Sets the ring's clock from this phone, then reads its logs and everything else it keeps. "
                            + "Stopping finishes the request the ring is answering and keeps what has come back."
                    )
                }

                if let trouble = model.syncTrouble {
                    Section("No luck") {
                        Text(trouble).font(.callout)
                    }
                }

                if !model.log.isEmpty {
                    Section("Progress") {
                        LogView(lines: model.log)
                    }
                }

                if let result = model.syncResult {
                    Section("Collected") {
                        ForEach(counts(of: result)) { count in
                            LabeledContent(count.kind, value: "\(count.count)")
                        }
                    }
                    Section("Ring") {
                        LabeledContent("Firmware", value: result.device.firmware ?? "—")
                        LabeledContent("Hardware", value: result.device.hardware ?? "—")
                        LabeledContent("Battery", value: battery(of: result.device))
                        LabeledContent("MTU", value: result.device.mtu.map { "\($0) bytes" } ?? "—")
                        LabeledContent("Can do", value: capabilities(of: result.device))
                    }
                    Section {
                        if result.failures.isEmpty {
                            Text("Nothing: the ring answered every request.")
                                .foregroundStyle(.secondary)
                        }
                        ForEach(Array(result.failures.enumerated()), id: \.offset) { _, failure in
                            VStack(alignment: .leading, spacing: 2) {
                                Text(failure.what)
                                Text(failure.why).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    } header: {
                        Text("Refused")
                    } footer: {
                        // A ring that refuses a request is not a failed sync: dialects
                        // differ, and the session carried on past each of these.
                        Text("A refusal is not a failure: the session went on past it.")
                    }
                }
            }
            .navigationTitle("Sync")
        }
    }

    /// One sample kind and how many of it a sync brought back.
    private struct Count: Identifiable {
        let kind: String
        let count: Int
        var id: String { kind }
    }

    private func counts(of result: SyncResult) -> [Count] {
        [
            Count(kind: "Heart rate", count: readings(in: result.hr)),
            Count(kind: "Steps", count: result.steps.count),
            Count(kind: "Blood oxygen", count: result.spo2.count),
            Count(kind: "HRV", count: result.hrv.count),
            Count(kind: "Stress", count: result.stress.count),
            Count(kind: "Temperature", count: result.temperature.count),
            Count(kind: "Sleep", count: result.sleep.count),
            Count(kind: "Workouts", count: result.workouts.count),
        ]
    }

    /// How many samples hold a reading: the ring's empty log slots come back as `.invalid`.
    private func readings(in samples: [HrSample]) -> Int {
        samples.count {
            switch $0.bpm {
            case .valid: true
            case .invalid: false
            }
        }
    }

    private func battery(of device: DeviceInfo) -> String {
        guard let percent = device.batteryPercent else { return "—" }
        guard let charging = device.charging else { return "\(percent)%" }
        return "\(percent)%, \(charging ? "charging" : "not charging")"
    }

    private func capabilities(of device: DeviceInfo) -> String {
        let can = [
            device.temperature ? "temperature" : nil,
            device.spo2 ? "spo2" : nil,
            device.newSleepProtocol ? "new sleep" : nil,
        ].compactMap { $0 }
        // A ring that never sent its bitmap has said nothing, which reads as no.
        return can.isEmpty ? "nothing it said so" : can.joined(separator: ", ")
    }
}

/// A session's log: oldest first, newest at the bottom, following the newest line.
///
/// Lives here rather than in a file of its own because the live screen shows the same log.
struct LogView: View {
    let lines: [LogLine]

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    ForEach(lines) { line in
                        Text(line.text)
                            .font(.system(.caption2, design: .monospaced))
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .id(line.id)
                    }
                }
            }
            .frame(height: 200)
            .onChange(of: lines.last?.id) { _, newest in
                guard let newest else { return }
                proxy.scrollTo(newest, anchor: .bottom)
            }
        }
    }
}
