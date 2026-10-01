# Local compatibility patch to block 0.1.6

Source: crates.io `block` 0.1.6, Steven Sheldon,
<https://github.com/SSheldon/rust-block>. The upstream manifest declares MIT;
the published archive and upstream repository do not include a separate license
file. Original author and license metadata are retained in Cargo.toml.

This dependency comes through Iced's Metal renderer (`wgpu-hal` / `metal`),
not the current CoreMIDI output driver.

The original `_NSConcreteStackBlock` foreign static used an empty enum, which
is uninhabited. Rust warns that this declaration will become an error. The
local patch declares the opaque external symbol as `c_void` and uses its raw
address without constructing a reference to foreign memory. The block `isa`
field remains a C-compatible pointer, and no symbol payload is read or written.
This matches the address-only use described by the
[Clang Blocks ABI](https://clang.llvm.org/docs/Block-ABI-Apple.html).

Bare `extern` declarations/functions now spell their existing `"C"` ABI
explicitly. The manifest explicitly retains edition 2015. Neither the public
API nor the package version changes; no lint is suppressed and no dependency
is downgraded.

This library-only copy omits upstream's test helper dependency, which is absent
from the published archive. Local integration tests exercise block invocation,
native stack-to-heap copying, reference cloning, and final capture disposal.
Run them on macOS with `cargo test -p block --test runtime`.

Remove this override when the renderer migrates off block 0.1.6 or an upstream
release includes the compatibility fix.
