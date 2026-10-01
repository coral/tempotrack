#include <ableton/Link.hpp>
#include <chrono>
#include <iomanip>
#include <iostream>
#include <thread>

// Silent interoperability probe built against Ableton's official C++ Link library.
// Columns: Unix microseconds, tempo, phase (quantum 4), peer count, is_playing.
// Used by the opt-in Rust integration test via TEMPOTRACK_LINK_REFERENCE.
int main() {
  ableton::Link link(120.);
  link.enableStartStopSync(false);
  link.enable(true);
  const auto end = std::chrono::steady_clock::now() + std::chrono::seconds(18);
  while (std::chrono::steady_clock::now() < end) {
    const auto state = link.captureAppSessionState();
    const auto at = link.clock().micros();
    const auto wall = std::chrono::duration_cast<std::chrono::microseconds>(std::chrono::system_clock::now().time_since_epoch()).count();
    std::cout << std::setprecision(12) << wall << ' ' << state.tempo() << ' ' << state.phaseAtTime(at, 4.) << ' ' << link.numPeers() << ' ' << state.isPlaying() << std::endl;
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
  }
  link.enable(false);
}
