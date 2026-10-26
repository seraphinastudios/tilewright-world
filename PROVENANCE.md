# Provenance

All code, documentation, tests and fixtures in this repository are MIT work by
Seraphina Studios. No outside implementation, tests or fixtures are copied or
adapted. Before any outside source is used as implementation reference or
reused, confirm its license is compatible with MIT and record the exact source,
revision, destination, transformation and notice here. GPL/AGPL material is
never copied, adapted or used as a dependency.

## Records

- **Variant hash constants.** The mixing function of the terrain-transition
  variant hash (`docs/spec/FORMAT.md` section 13,
  `crates/tilewright-source/src/transitions/resolve.rs`) is the output
  function of SplitMix64. Source: Sebastiano Vigna's `splitmix64.c`
  (<https://prng.di.unimi.it/splitmix64.c>, 2015), whose shifts and
  multipliers (30, `0xbf58476d1ce4e5b9`, 27, `0x94d049bb133111eb`, 31) are
  David Stafford's "Mix13". Only these five constants are used; the function
  is written here from them, and no code is copied. Notice: the author
  dedicated `splitmix64.c` to the public domain under CC0 1.0.
- **musl's copyright notice.** `tools/release/licenses/musl-COPYRIGHT` is
  musl's `COPYRIGHT` file at tag `v1.2.5`
  (<https://git.musl-libc.org/cgit/musl/plain/COPYRIGHT?h=v1.2.5>, SHA-256
  `f9bc4423732350eb0b3f7ed7e91d530298476f8fec0c6c427a1c04ade22655af`),
  copied unchanged. `tools/release/package.py` reproduces it in the Linux
  release archives, whose binaries link musl statically. Notice: musl is
  MIT licensed, as the file states.
