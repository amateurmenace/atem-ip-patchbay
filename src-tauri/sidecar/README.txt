FFmpeg sidecar lives here when this binary is built for distribution.

For dev builds (`cargo tauri dev`) the bundling is skipped, so this
directory only contains this README. Local `cargo tauri build` runs
either need an `ffmpeg` binary dropped in here, or rely on the
ffmpeg_path resolver falling through to PATH (any `brew install ffmpeg`
already on the dev machine).

CI populates this directory before `cargo tauri build` with our own
FFmpeg build (see .github/workflows/build-ffmpeg.yml and
ci/ffmpeg-pins.env): FFmpeg n8.1.1 configured with libsrt, libx264,
libx265 and the DeckLink output device (built against Blackmagic
DeckLink SDK 16.0), plus VideoToolbox on macOS and NVENC on Windows.
The Mac build carries its Homebrew dylibs next to the binary; the
Windows build carries the MinGW runtime DLLs. The NDI runtime, libomt,
and (on Windows) atem-net-diag.exe are staged here as well.

The presence of this README is load-bearing for `bundle.resources`'s
`sidecar/*` glob — without at least one file matching, tauri-bundler's
build hook fails with "glob pattern sidecar/* path not found or didn't
match any files." Don't delete it.
