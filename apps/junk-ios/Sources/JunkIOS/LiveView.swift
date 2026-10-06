import SwiftUI

/// A workout on the ring, its heart rate as it arrives, and the record the ring kept.
struct LiveView: View {
    @Bindable var model: RingModel

    var body: some View {
        NavigationStack {
            List {
                Section {
                    LabeledContent("Sport type") {
                        TextField("7", value: $model.sportType, format: .number)
                            .keyboardType(.numberPad)
                            .multilineTextAlignment(.trailing)
                    }
                    LabeledContent("Seconds") {
                        TextField("60", value: $model.seconds, format: .number)
                            .keyboardType(.numberPad)
                            .multilineTextAlignment(.trailing)
                    }
                    HStack {
                        Button("Start") {
                            Task { await model.startWorkout() }
                        }
                        .disabled(model.running)
                        Spacer()
                        Button("Stop") {
                            model.stopWorkout()
                        }
                        .disabled(!model.running)
                    }
                    .buttonStyle(.bordered)
                } footer: {
                    Text("Stop ends the stream, not the workout: the ring is still told to stop, and the record it stores is still fetched. Sport 7 is the one `junk live` starts.")
                }

                Section("Heart rate") {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        switch model.bpm {
                        case let .valid(bpm):
                            Text(String(bpm))
                                .font(.system(size: 72, weight: .semibold, design: .rounded))
                                .monospacedDigit()
                                .contentTransition(.numericText())
                            Text("bpm").font(.title3).foregroundStyle(.secondary)
                        case .invalid:
                            Text(model.running ? "measuring…" : "—")
                                .font(.system(size: 36, weight: .semibold, design: .rounded))
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if model.running { ProgressView() }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }

                if let trouble = model.liveTrouble {
                    Section("No luck") {
                        Text(trouble).font(.callout)
                    }
                }

                if !model.log.isEmpty {
                    Section("Progress") {
                        LogView(lines: model.log)
                    }
                }

                if let result = model.liveResult {
                    workout(of: result)
                }
            }
            .navigationTitle("Live")
        }
    }

    @ViewBuilder
    private func workout(of result: LiveResult) -> some View {
        if let workout = result.workout {
            Section("Workout") {
                LabeledContent("Sport", value: "\(workout.record.sportType)")
                LabeledContent("Lasted", value: seconds(workout.record.durationS))
                LabeledContent("Heart rate", value: rates(of: workout.record))
                LabeledContent("Calories", value: number(workout.record.calories))
                LabeledContent("Distance", value: number(workout.record.distance))
                LabeledContent("Steps", value: number(workout.record.steps))
                LabeledContent(
                    "Detail",
                    value: "\(workout.heartRates.count) rates, one every \(workout.sampleSeconds)s"
                )
            }
        } else {
            Section("Workout") {
                Text("The ring stored none: a workout under about a minute is not kept.")
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func seconds(_ value: UInt64?) -> String {
        value.map { "\($0)s" } ?? "—"
    }

    private func number(_ value: UInt64?) -> String {
        value.map(String.init) ?? "—"
    }

    /// The record's three rates on one line, in the ring's own order: low, average, high.
    private func rates(of record: WorkoutRecord) -> String {
        let rates = [record.rateMin, record.rateAvg, record.rateMax]
        guard rates.contains(where: { $0 != nil }) else { return "—" }
        return rates.map { $0.map(String.init) ?? "—" }.joined(separator: " / ")
    }
}
