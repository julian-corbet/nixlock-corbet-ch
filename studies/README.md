# studies

Written-up findings: things that were tried in
[`../experiments/`](../experiments/README.md), worked (or failed instructively),
and are worth recording properly -- with the reasoning, not just the result. A
study earns its place here once it changed a decision in the project.

One finding is seeded below because it already shaped a decision -- the render
path and the three-item `buildInputs`. It is written as the reasoning plus the
measurement that would confirm it; the measurement itself (an `ldd` over the
built binary, a closure-size number) is the write-up still owed.

## CPU / `wl_shm` rendering keeps Mesa out of the closure

**The decision it shaped.** nixlock renders on the CPU -- tiny-skia into a
`wl_shm` buffer -- and touches no EGL/GL/Vulkan. As a direct consequence
`package.nix`'s `buildInputs` is exactly three C libraries: `wayland`,
`libxkbcommon`, `pam`. Nothing graphical-driver-shaped is in that list, and a
GPU renderer would have forced at least Mesa into it.

**Why that is worth a study and not just a comment.** A graphical binary built
from nixpkgs and run on a *foreign* distro (an Arch/CachyOS host applying config
with system-manager + home-manager, which is a first-class target for this
family) hits an ABI wall: the binary would be linked against nixpkgs' OWN Mesa,
which cannot see the host's real GPU driver stack, and GPU-touching calls fail on
an otherwise healthy box. The usual fixes are ugly -- point the binary at the
system's Mesa, add options whose only job is to undo the module's own default, or
just accept it only works on NixOS. A CPU/`wl_shm` locker sidesteps the wall
entirely: there is no GL context to mislink, so the same build runs on NixOS and
on a foreign-distro wlroots session unchanged, which is precisely the portability
this family is built around. That is why the render path is not merely an
implementation detail -- it is what lets one derivation serve every plane.

**What is still owed.** The measurement, not the reasoning: `ldd` (or a closure
walk) over the built `nixlock` showing no `libEGL`/`libGLESv2`/`libvulkan`/Mesa,
and the closure size next to a hypothetical GL build, captured once and recorded
here. Until then this is a sound structural argument with the confirming number
still to take.
