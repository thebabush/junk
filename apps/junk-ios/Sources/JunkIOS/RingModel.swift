import Observation

/// One line of a session's log.
struct LogLine: Identifiable {
    let id: Int
    let text: String
}

/// What the three screens share: the ring that was picked, the log a session writes as it
/// runs, and what the last call came back with.
///
/// Every call here is one of the three `junk-ffi` exports awaited from the main actor. They
/// are `async` already and do their waiting off this thread, so there is no queue, no
/// dispatch and no Bluetooth state machine to keep — the Rust has all of that.
@MainActor
@Observable
final class RingModel {
    /// How long a call listens for its ring before giving up, in seconds. One number for
    /// all three: the app has no reason to make it a setting.
    static let timeoutSecs: UInt64 = 10

    /// The most log lines kept. A live session writes one a second and a sync one per
    /// request, so the log is bounded rather than left to grow for as long as the app runs.
    static let logLimit = 400

    /// What the phone can say for itself on the simulator, which has no Bluetooth radio and
    /// so can neither find a ring nor, as it turns out, admit that it cannot.
    static let needsADevice =
        "The simulator has no Bluetooth. Run this on an iPhone to reach a ring."

    // Scan.
    var rings: [FoundRing] = []
    /// The `device` argument the other two calls pass: a ring's own id, or `nil` for
    /// whichever single ring is in range.
    var selectedID: String?
    var scanning = false
    var scanTrouble: String?

    // Sync.
    var days: UInt8 = 1
    var syncing = false
    var syncResult: SyncResult?
    var syncTrouble: String?

    // Live.
    var sportType: UInt8 = 7
    var seconds: UInt64 = 60
    var running = false
    /// The last heart rate the ring streamed; `.invalid` until the sensor has a reading.
    var bpm: Bpm = .invalid
    var liveResult: LiveResult?
    var liveTrouble: String?

    /// What the running session has reported, oldest first.
    private(set) var log: [LogLine] = []
    private var nextLineID = 0

    /// The button that ends the running live stream, for as long as one is running.
    private var stop: Stop?
    /// The button that ends the sync in progress, while one is running.
    private var syncStop: Stop?

    /// The ring in range, as `scan` saw them.
    func findRings() async {
        guard !scanning else { return }
        scanning = true
        scanTrouble = nil
        defer { scanning = false }
        do {
            rings = try await scan(timeoutSecs: Self.timeoutSecs)
            // A ring that is no longer in range is no longer a choice.
            if let selectedID, !rings.contains(where: { $0.id == selectedID }) {
                self.selectedID = nil
            }
        } catch {
            rings = []
            scanTrouble = Self.explain(error)
        }
    }

    /// `days` days of the selected ring's logs, with every step reaching ``log``.
    func syncRing() async {
        guard !syncing else { return }
        syncing = true
        syncTrouble = nil
        syncResult = nil
        clearLog()
        defer { syncing = false }
        let stop = Stop()
        syncStop = stop
        defer { syncStop = nil }
        do {
            syncResult = try await sync(
                device: selectedID,
                days: days,
                timeoutSecs: Self.timeoutSecs,
                stop: stop,
                observer: SessionLog(model: self)
            )
        } catch {
            syncTrouble = Self.explain(error)
        }
    }

    /// Ends the sync after the request the ring is answering. Not an abort: what has been
    /// collected so far comes back as the result, so a stopped sync still shows its days.
    func stopSync() {
        syncStop?.stop()
    }

    /// A workout on the ring, streaming its heart rate until the seconds are up or
    /// ``stopWorkout()`` is pressed.
    func startWorkout() async {
        guard !running else { return }
        running = true
        liveTrouble = nil
        liveResult = nil
        bpm = .invalid
        clearLog()
        let stop = Stop()
        self.stop = stop
        defer {
            running = false
            self.stop = nil
        }
        do {
            liveResult = try await live(
                device: selectedID,
                sportType: sportType,
                seconds: seconds,
                timeoutSecs: Self.timeoutSecs,
                stop: stop,
                observer: SessionLog(model: self)
            )
        } catch {
            liveTrouble = Self.explain(error)
        }
    }

    /// Ends the stream early. Not an abort: the session still stops the workout on the ring
    /// and fetches the record it stored, so `live` returns a result like any other.
    func stopWorkout() {
        stop?.stop()
    }

    /// One step of a running session, already turned into words off the main actor.
    fileprivate func observed(_ line: String, bpm: Bpm?) {
        log.append(LogLine(id: nextLineID, text: line))
        nextLineID += 1
        if log.count > Self.logLimit {
            log.removeFirst(log.count - Self.logLimit)
        }
        if let bpm {
            self.bpm = bpm
        }
    }

    private func clearLog() {
        log.removeAll()
    }

    /// One `Progress` as one line. The words are the ones `junk-app` chose and `junk-cli`
    /// prints; all that differs is where the line goes.
    ///
    /// `nonisolated`, because the observer calls it on the session's own thread: it reads
    /// nothing of the model, and the point is to do this work before hopping.
    nonisolated static func line(for progress: Progress) -> String {
        switch progress {
        case let .connected(channels, mtu):
            "connected, mtu \(mtu): \(channels.joined(separator: ", "))"
        case let .asking(what):
            "-> \(what)"
        case let .answered(what, summary):
            "<- \(what): \(summary)"
        case let .failed(what, why):
            "!! \(what): \(why)"
        case let .event(text):
            ".. \(text)"
        case let .workoutSample(seq, bpm):
            switch bpm {
            case let .valid(bpm): "hr #\(seq): \(bpm) bpm"
            case .invalid: "hr #\(seq): no reading yet"
            }
        case let .linkError(why):
            "!! link: \(why)"
        case .workoutStoreTimeout:
            "!! the ring never said the workout was stored; fetching it anyway"
        case .interrupted:
            ".. stopped early; the workout is still being closed on the ring"
        case .disconnected:
            ".. disconnected"
        }
    }

    /// Why a call failed, in a sentence to put on screen.
    ///
    /// A radio that is not there at all — the simulator's case — says so plainly and adds
    /// that only a real phone can reach a ring.
    private static func explain(_ error: Error) -> String {
        guard let error = error as? JunkError else { return String(describing: error) }
        return switch error {
        case let .RadioOff(message, switchable):
            switchable ? message : "\(message)\n\n\(needsADevice)"
        case let .Bluetooth(message): message
        case let .NoRing(message, _): message
        case let .Session(message): message
        }
    }
}

/// The session's own eyes on the app: `junk-ffi` hands one of these every step.
///
/// `Observer` is `Sendable` and `step` is called from the session's task, not the main one,
/// and it must not block. So the step becomes a line and a number here, on whatever thread
/// called, and only those plain values hop to the main actor.
private final class SessionLog: Observer {
    private let model: RingModel

    init(model: RingModel) {
        self.model = model
    }

    func step(progress: Progress) {
        let line = RingModel.line(for: progress)
        let bpm: Bpm? =
            switch progress {
            case let .workoutSample(_, bpm): bpm
            default: nil
            }
        Task { @MainActor [model] in
            model.observed(line, bpm: bpm)
        }
    }
}
