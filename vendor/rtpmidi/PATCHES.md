# Local patch to rtpmidi 0.4.4

Source: <https://crates.io/crates/rtpmidi/0.4.4>, upstream repository
<https://github.com/iKadmium/rtp-midi-rs>.

This is the latest published version at implementation time, retaining upstream's
GPL-3.0-or-later license and original license text in LICENSE.md.

The MIDI message encoder recognized TimingClock's F8 status byte but panicked
when writing its empty payload. The local change adds the missing no-payload
case. TempoTrack's RTP output tests encode exactly F8 and exercise invitation,
clock delivery, and socket cleanup against a local receiver.

The manifest omits upstream development profiles and packaging references to
examples/tests/readme files not included in this library-only copy. No dependency
versions are downgraded. TempoTrack uses its own mdns-sd dependency; this crate's
optional mdns feature remains disabled.

Unused private control-marker wrappers were removed, and borrowed MIDI parser
and iterator return types now state their elided lifetimes explicitly. These
compiler-warning fixes do not change packet encoding or parsing behavior.

Remove this patch when an upstream release includes the TimingClock encoder fix.
