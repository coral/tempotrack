# TempoTrack patches to ableton-link-rs 0.1.2

Source: crates.io ableton-link-rs 0.1.2 (anweiss/ableton-link-rs).

The published manifest has an unconditional rodio 0.21.1 dependency used only by
its rusthut example, not by the library. Rodio pulls CPAL 0.16 and alsa-sys 0.3,
which conflicts with TempoTrack CPAL 0.18 / alsa-sys 0.4 (both link libasound).
This library-only package omits that demo dependency, example targets, and dev-only
console-subscriber. It preserves the upstream version and runtime dependencies;
no application dependency is downgraded. Upstream LICENSE is retained.

The active IPv4 messenger now joins the loopback multicast group and mirrors
multicast announcements and goodbye messages to loopback. Official Ableton Link
uses a separate loopback discovery interface for applications on the same host;
joining only the default physical interface prevented local interoperability.
Normal outbound announcements still use the default network interface. This
patch does not claim dynamic multi-interface or IPv6 support.

Discovery sends from a separate ephemeral UDP socket and listens for replies on
that socket as well as the shared multicast port. This matches the official
implementation and avoids relying on which process receives unicast replies to
the shared port 20808. Clock measurement binds an unspecified source address
before connecting, allowing the kernel to select a route to loopback or a
physical network peer.

BasicLink construction no longer installs a global stdout tracing subscriber.
Logging configuration belongs to the embedding application; installing a global
subscriber corrupted machine-readable output such as the timing probe's JSON.
The discovery gateway also no longer installs a process-global Ctrl-C handler;
TempoTrack owns process termination, explicit disable, and runtime teardown.

An opt-in test checks sustained peer membership, a live tempo change, phase,
and absence of transport control against an official C++ Ableton Link peer using
tools/link_reference.cpp. Constant-tempo reception alone is insufficient because
a disconnected peer can keep freewheeling. Sustained interoperability remains
unverified in the development environment: a reference-only C++ control also
retained its initial tempo, and local firewall permissions are being checked.
The application adapter aligns the audio clock once when the first peer session
is joined, then bounds subsequent phase changes to 2 ms.

Remove these patches when an upstream release provides the packaging,
discovery socket, measurement routing, logging, and signal-ownership fixes.
