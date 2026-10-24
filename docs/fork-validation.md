# Local validation of the motion MCP fork

Validated on Windows x64, 2026-10-24, with Rust 1.95.0. This records observed results,
not a claim of After Effects compatibility or production readiness for every shot.

## Verified workflows

- Real stdio MCP discovery: 38 tools, including 11 fork additions; the native registry
  reports 734 commands and 307 built-in effects. Registry counts do not prove fidelity.
- All six starter recipes render changing frames with Cyrillic text. The card and
  screen recipes use imported, playing procedural H.264 footage, not placeholder stills.
- Separate native scenes exercise spatial planes with a moving camera and an animated
  ellipse mask revealing playing footage. Starter recipes themselves remain 2D.
- Native Render Queue exports H.264 and ProRes 4444 RGBA. Independent decoding confirms
  24 changing frames at 12 FPS, two seconds duration, and actual transparent and opaque
  pixels in the ProRes output.
- The editable `.ecproj` reopens in a fresh server process with identical control-frame
  pixels and no missing source. The native UI also opens and displays the project.
- The installed MCP executable is tested with headless auto-save enabled. Atomic edits,
  undo/redo, source preservation, effect UID collisions, literal batch-reference strings,
  invalid inputs and offset/stretched-layer time conversion have regression coverage.

The local test footage is an original procedural fixture. No Adobe plug-ins, presets,
reference screenshots or proprietary media libraries are distributed in this fork.

## Known GPU verification limitation

On the local **NVIDIA GeForce RTX 4060 (Vulkan)**, the existing test
`tests_adv3d::gpu_matches_cpu_wide_penumbrae` reports **15 of 4159** shadowed pixels
outside its 2/255 error tolerance; its allowed count is approximately eight. It also
fails when run alone with `EFFECTCRAFT_REQUIRE_GPU=1`, so this is not a skipped test or
a result inferred from an unavailable adapter. Explicit Vulkan selection reproduces it.
The other 231 tests in that GPU target pass in the workspace run; one benchmark is ignored.

No GPU shader, test threshold or skip condition was changed to hide this discrepancy.
The exact numerical cause remains unproven. CPU/GPU projection, shadow sampling and
interpolation rounding are candidates, not established diagnoses.

The MCP is installed in the **CPU mode used by the verified delivery workflow**.
GPU acceleration remains optional; validate actual shots and compare their rendered
frames before using it. Full `cargo xtask ci` therefore must not be described as green
on this local GPU while that test fails. Other machines/backends may behave differently.

Direct3D 12 with the local default FXC compiler also rejects an existing `classic3d`
compute pipeline during initialization. It is not a verified workaround for Vulkan here.

## Reproduce the checks

```text
cargo test -p effectcraft-automation
cargo xtask ci
```

For an explicit GPU check, set `EFFECTCRAFT_REQUIRE_GPU=1` and run the exact failing
test in `effectcraft-gpu`. That environment flag converts unavailable GPU initialization
into a failure, preventing a false successful result from an adapter-dependent skip.

The workspace test suite also checks native exports and save/reopen behavior. A passing
structural audit or a single preview frame does not replace these checks for new shots.
