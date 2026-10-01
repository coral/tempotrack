# Clock outputs

TempoTrack distributes its stabilized audio clock through Ableton Link, OSC,
local MIDI Clock, and RTP-MIDI. All four can run at once. OSC supports multiple
destinations; MIDI supports multiple ports plus a virtual port.

These outputs carry tempo and clock alignment. They do not send MIDI Start,
Stop, Continue, or Song Position Pointer, and Link Start/Stop synchronization is
disabled. A receiver that requires a Start message before accepting MIDI Clock
needs its own playback control.

## Desktop settings

Open **Settings → Outputs**, enable the outputs you want, and open each output's
page to choose destinations. Click **Apply settings** to save and activate the
changes. **Back** discards unapplied edits. Output-only changes preserve the
current audio input and beat lock.

- **Ableton Link:** enable it and check peer count in its settings page.
- **OSC:** add each receiver as `host:port` or `[IPv6]:port`. The default address
  prefix is `/tempotrack`.
- **MIDI Clock:** select output ports, or enable **TempoTrack Clock · virtual
  port** and select that input in another application. Use Refresh after adding
  devices. Disconnected selected ports remain visible so they can be removed.
- **Network MIDI:** choose the session name and control port. The default
  session is `TempoTrack`, with control/data ports `5004` and `5005`.

Configuration and per-destination status stay in settings. New installations and
older settings files start with these four outputs disabled.

## Headless usage

Find device names and MIDI IDs first:

```sh
cargo run --no-default-features -- --list-inputs
cargo run --no-default-features -- --list-midi-outputs
cargo run --no-default-features -- --list-outputs
```

Send all four outputs simultaneously, including two OSC destinations:

```sh
cargo run --release --no-default-features -- \
  --input "Audio Interface" \
  --output link --output osc --output midi --output rtpmidi \
  --osc-target 192.168.1.20:7000 \
  --osc-target 192.168.1.21:7000 \
  --midi-virtual \
  --rtpmidi-name TempoTrack --rtpmidi-port 5004
```

Use repeated `--midi-port "Exact port name"` or `--midi-port-id "ID"` for physical
or existing local MIDI destinations. Do not select the same destination twice.
Saved GUI selections include both ID and name: reconnection prefers the ID,
then falls back to an unambiguous exact name. An ID-only CLI selection has no
name fallback. Ambiguous names are rejected.

`--osc-prefix /clock` changes all OSC addresses. OSC options require
`--output osc`, MIDI options require `--output midi`, and network MIDI options
require `--output rtpmidi`.

Headless runs use CLI configuration rather than saved desktop preferences.
Without `--output`, headless mode uses `stdout`. Add `--output stdout` alongside
network outputs for text monitoring; `--output none` must be used alone. Use
`--gui` explicitly to open the desktop with CLI overrides.

## Resolume and Link

On the same network, enable Link in TempoTrack. In Resolume, select **View →
Show Ableton Link**, then enable the Link toolbar button. Resolume automatically
joins discovered peers and synchronizes BPM and position in the measure.
See [Resolume's Link setup](https://www.resolume.com/support/en/link).

TempoTrack publishes its audio-derived tempo and beat alignment; incoming Link
tempo changes do not change its detector. While tracking, it continues
publishing the audio clock. Detected beats per bar define the Link quantum,
with four as the fallback. Downbeats align with quantum boundaries. Initial
acquisition establishes alignment; subsequent phase adjustments are bounded.

Generic OSC addresses below are not a Resolume-specific clock protocol. They
require receiver-side mapping; Link is the intended automatic Resolume path.
Interoperability testing with an official Link peer is still in progress;
end-to-end validation with Resolume has not yet been completed.

## OSC protocol

OSC uses UDP and immediate bundles (timetag `0,1`). Messages emitted together
share a bundle; receivers do not need synchronized wall clocks. Each destination
gets the following addresses, using `/tempotrack` as the default prefix:

| Address | OSC type | Value and cadence |
| --- | --- | --- |
| `/tempotrack/bpm` | Float (`f`) | Effective output-clock BPM, 30 Hz while active |
| `/tempotrack/phase` | Float (`f`) | Beat phase in `[0,1)`, 30 Hz while active |
| `/tempotrack/beat` | Int32 (`i`) | Beat counter at each emitted beat boundary |
| `/tempotrack/bar` | Int32 (`i`) | Bar counter at each emitted bar boundary, when a bar grid exists |
| `/tempotrack/atom` | Int32 (`i`) | Subdivision counter; four subdivisions per beat |
| `/tempotrack/active` | Int32 (`i`) | `1` for an active clock, otherwise `0`; on change and once per second |

Counters start at zero on the first emitted boundary of each kind in a new
tracking generation. They count positions on the clock grid, so skipped pulses
may produce gaps. They are not song positions. `/active 0` is also sent when the
output is disabled normally. Pulse messages are not limited to the 30 Hz
telemetry cadence.

The reported BPM comes from the clock interval, including bounded phase
correction, so it can differ slightly from the main display's estimated BPM.
UDP status reports sending or socket errors; successful sends do not establish
that a receiver is listening.

## Local MIDI and RTP-MIDI

Both send only MIDI Timing Clock (`0xF8`) at **24 pulses per quarter note**.
Clock has no MIDI channel and does not encode bar position. Local MIDI uses
CoreMIDI on macOS and ALSA MIDI on Linux. Virtual outputs remain available even
when no application is listening.

RTP-MIDI is an IPv4 listener. It accepts incoming session invitations and sends
clock to every connected participant; it does not initiate invitations or use
incoming MIDI to control TempoTrack. Connect to the advertised session from
your receiver's network MIDI setup.

The configured control port and the following data port must both be available.
After binding them, TempoTrack advertises `_apple-midi._udp.local.` through
mDNS. Disabling the output removes its advertisement and closes the session.
Settings show the advertised name, including a discovery conflict rename, and
connected participants. A session goodbye is connection cleanup, not a musical
Stop command.

## Clock continuity and failures

Outputs project the stabilized beat grid using monotonic time and the global
Offset control. They do not depend on UI refresh or rounded BPM. Missed deadlines
are discarded instead of replaying a burst of historical ticks.

Valid holdover continues the clock. Silence gating, stopped tracking, or stale
capture suspends clock publication. Link peers can keep their own clocks running
at the last published tempo; TempoTrack does not remotely stop them.

Destinations have independent workers. Failed destinations show their error and
retry after 1, 2, then 5 seconds, without stopping other outputs or audio
tracking. Explicit local MIDI destinations are checked for disappearance even
while the clock is inactive. Link and RTP-MIDI each own their runtime; capture
and tracking remain synchronous and use bounded ring buffers.

## Timing diagnostic and integration tests

`tools/output_probe.rs` generates a synthetic clock without audio capture or
model inference. Run its `output-probe` binary with the `offline` tools feature:

```sh
cargo run --release --no-default-features --features offline \
  --bin output-probe -- --bpm 126.3 --seconds 30 --load-workers 4
```

The JSON report on stdout includes tick count, missed and duplicate/backward
ticks, deadline lateness, absolute interval jitter, and beat-phase error.
Distributions include median, p95, p99, and maximum. Lateness and jitter are in
milliseconds; beat-phase error is in beats.

To exercise real destinations concurrently, append the same output and
destination flags supported by TempoTrack:

```sh
cargo run --release --no-default-features --features offline \
  --bin output-probe -- --seconds 30 --output osc --output midi \
  --osc-target 127.0.0.1:7000 --midi-virtual
```

This measures the probe's **software output-worker deadlines**, not network
packet arrival, MIDI wire timing, or a receiver's rendered beat. Enabling real
outputs alongside it adds their workload but does not turn the report into an
end-to-end latency measurement. Receive-side instrumentation is needed for that.

The real virtual-MIDI loopback and reconnect tests are opt-in because they need
a running OS MIDI service:

```sh
cargo test --no-default-features --lib virtual_midi -- --ignored --nocapture
```

These passed with CoreMIDI, including exact timing-only bytes and endpoint
cleanup. Physical MIDI hardware and Linux ALSA have not yet been exercised.

## Official Link interoperability probe

The opt-in Link test uses a silent peer built against
[Ableton's official C++ implementation](https://github.com/Ableton/link), rather
than a second instance of the Rust implementation. The reference checkout and
binary stay under ignored `target/`; they are not application dependencies.
The reference revision used during development is
`9c9091275e707ab09d09a5a608fcdb84bf0dec85`.

Build on macOS:

```sh
git clone --depth 1 --recurse-submodules --shallow-submodules \
  https://github.com/Ableton/link.git target/link-reference
clang++ -std=c++17 -O2 -DLINK_PLATFORM_MACOSX=1 \
  -I target/link-reference/include \
  -I target/link-reference/modules/asio-standalone/asio/include \
  tools/link_reference.cpp -o target/link-reference/tempo_probe
TEMPOTRACK_LINK_REFERENCE=target/link-reference/tempo_probe \
  cargo test --no-default-features --lib interoperates_with_official_link_reference \
  -- --ignored --nocapture
```

On Linux, use `c++`, `-pthread`, and `-DLINK_PLATFORM_LINUX=1` in place of the
macOS compiler/platform flag. The probe reports tempo, phase, peer count, and
transport state; the Rust test checks sustained discovery, an audio-derived tempo
change from 131.7 to 137.3 BPM with continuous beat phase, phase alignment, and
that no Start/Stop synchronization occurs. A peer retaining an initial BPM does
not pass: it might simply be freewheeling after losing the connection.

These tests open LAN multicast and unicast sockets. Both the application and the
reference probe need firewall permission. Rebuilding either executable can
change its identity and trigger another permission prompt in Little Snitch.
Run them on a test network without an active Link performance. Sustained Link
interoperability has not yet been verified in this development environment: the
reference-only C++ control also retained its initial tempo after a change, with
firewall permissions still under investigation.

The local RTP-MIDI integration test opens UDP and mDNS sockets, performs both
AppleMIDI invitations, receives clock packets, and checks port release:

```sh
cargo test --no-default-features --lib listener_accepts_invitations_and_sends_only_clock \
  -- --ignored --nocapture
```
