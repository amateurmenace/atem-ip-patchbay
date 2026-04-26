FFmpeg sidecar lives here when this binary is built for distribution.

For dev builds (`cargo tauri dev`) the bundling is skipped, so this
directory only contains this README. Local `cargo tauri build` runs
either need an `ffmpeg` binary dropped in here, or rely on the
ffmpeg_path resolver falling through to PATH (any `brew install ffmpeg`
already on the dev machine).

CI populates this directory before `cargo tauri build`:

  Mac arm64:  Jellyfin GPL build (jellyfin-ffmpeg_*_portable_macarm64-gpl)
  Win x64:    BtbN GPL build (ffmpeg-n*-latest-win64-gpl-8.1)

Both ship libsrt + HEVC + libx264 + the platform's hardware-accelerated
encoders (VideoToolbox / nvenc / qsv) statically linked, so the bundled
binary has no DLL or dylib dependencies of its own.

The presence of this README is load-bearing for `bundle.resources`'s
`sidecar/*` glob — without at least one file matching, tauri-bundler's
build hook fails with "glob pattern sidecar/* path not found or didn't
match any files." Don't delete it.
