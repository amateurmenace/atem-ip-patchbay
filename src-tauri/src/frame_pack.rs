//! Stride-strip helper shared between the NDI capture path
//! (`ndi_capture.rs`) and the OMT capture path (`omt_capture.rs`).
//! Both grafton-ndi and libomt hand us frames whose rows can be
//! padded for SIMD alignment — `line_stride > width * bpp`. FFmpeg's
//! `-f rawvideo -s WxH -pix_fmt FMT` demuxer reads exactly
//! `width * bpp * height` bytes and assumes tight packing; padded
//! input produces a row-shift corruption that's deeply confusing
//! (no error, video just slowly tears diagonally).
//!
//! Extracted into its own module in alpha.9 (Phase B) so a future
//! bug fix to the stride logic flows to both NDI + OMT capture
//! paths automatically. Previously inlined in `ndi_capture.rs`.
//!
//! The function is intentionally source-agnostic: it takes a raw
//! `&[u8]` buffer + dimensions + bpp + optional stride. Source-
//! specific enum-to-bpp mapping (NDI's `PixelFormat`, OMT's
//! `OmtCodec`) stays in the respective capture modules, since
//! each SDK uses its own type and we don't gain anything by
//! introducing a third intermediate enum here.

/// Tightly pack a frame so each row is exactly `width * bpp` bytes
/// with no trailing padding.
///
/// # Arguments
///
/// * `data` — raw frame buffer from the SDK; may include per-row padding
///   for SIMD alignment, plus optional trailing alignment at the buffer's tail.
/// * `width`, `height` — pixel dimensions.
/// * `bpp` — bytes per pixel (4 for BGRA/RGBA, 2 for UYVY/YUY2).
/// * `line_stride` — bytes per row in `data`. `None` means
///   "stride is unknown or this is compressed/opaque — pass through";
///   `Some(n)` enables the actual repack logic.
///
/// # Returns
///
/// A `Vec<u8>` of length `width * bpp * height` when repacking happens,
/// or a clone of `data` truncated to the expected length when the
/// source is already tight.
///
/// # Performance
///
/// Fast paths (most senders, including iPhone NDICAM at 1280×720, OMT
/// senders at common resolutions): one memcpy total — the row-by-row
/// loop is skipped because `actual_stride == expected_stride`. Only
/// senders that pad rows (some desktop NDI tools at non-power-of-two
/// widths) hit the slow path with H separate row copies.
///
/// # Bounds
///
/// `data.len()` is allowed to be larger than `actual_stride * height`
/// (some SDKs include trailing alignment); we slice to the expected
/// length. `data.len() < actual_stride * height` is logged and the
/// loop bails at the bad row to avoid OOB reads.
pub fn pack_frame(
    data: &[u8],
    width: usize,
    height: usize,
    bpp: usize,
    line_stride: Option<usize>,
) -> Vec<u8> {
    let expected_stride = width * bpp;
    let expected_total = expected_stride * height;

    let Some(actual_stride) = line_stride else {
        // No stride info — the SDK gave us either compressed data
        // (NDI's DataSizeBytes variant, OMT's VMX1/FPA1 codecs) or
        // a layout we don't recognize. Pass through; downstream is
        // either piping to FFmpeg as compressed input or it'll fail
        // loudly with a clear pix_fmt mismatch error.
        return data.to_vec();
    };

    if actual_stride == expected_stride {
        // Already tight. The buffer may still be longer than needed
        // (SDK-internal trailing alignment); slice to the exact
        // expected length so FFmpeg doesn't see stray bytes.
        let take = expected_total.min(data.len());
        return data[..take].to_vec();
    }

    if actual_stride < expected_stride {
        // Underflow stride is nonsensical for uncompressed frames
        // (would mean rows overlap). Log and pass through; if this
        // fires, the SDK's reporting an incorrect stride and we'd
        // rather show torn video than crash.
        log::warn!(
            "frame_pack: line_stride={actual_stride} < expected={expected_stride} \
             for {width}x{height}@{bpp}bpp; passing through unpacked"
        );
        return data.to_vec();
    }

    // Padded — copy each row's first `expected_stride` bytes into a
    // tight buffer.
    let mut packed = Vec::with_capacity(expected_total);
    for row in 0..height {
        let row_start = row * actual_stride;
        let row_end = row_start + expected_stride;
        if row_end > data.len() {
            log::warn!(
                "frame_pack: row {row} out of bounds: row_end={row_end} > data.len={} \
                 (stride={actual_stride}, expected_stride={expected_stride}, height={height})",
                data.len()
            );
            break;
        }
        packed.extend_from_slice(&data[row_start..row_end]);
    }
    packed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_no_stride_info() {
        let buf = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let out = pack_frame(&buf, 2, 1, 4, None);
        assert_eq!(out, buf);
    }

    #[test]
    fn fast_path_when_already_tight() {
        // 4x2 BGRA = 32 bytes, stride=16 (tight)
        let buf: Vec<u8> = (0..32).collect();
        let out = pack_frame(&buf, 4, 2, 4, Some(16));
        assert_eq!(out, buf);
    }

    #[test]
    fn slices_off_trailing_alignment() {
        // 4x2 BGRA = 32 bytes expected, but buffer is 40 (8 bytes trailing)
        let buf: Vec<u8> = (0..40).collect();
        let out = pack_frame(&buf, 4, 2, 4, Some(16));
        assert_eq!(out.len(), 32);
        assert_eq!(out, (0..32).collect::<Vec<u8>>());
    }

    #[test]
    fn slow_path_strips_per_row_padding() {
        // 2x3 BGRA: width*bpp=8 bytes, stride=12 bytes (4 trailing per row)
        // 3 rows * 12 bytes = 36 input; expect 24 output.
        let mut buf = Vec::with_capacity(36);
        for row in 0..3u8 {
            buf.extend_from_slice(&[row, row, row, row, row, row, row, row]); // 8 data bytes
            buf.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);                  // 4 padding bytes
        }
        let out = pack_frame(&buf, 2, 3, 4, Some(12));
        assert_eq!(out.len(), 24);
        // Row 0: [0,0,0,0,0,0,0,0]; row 1: [1,1,...]; row 2: [2,2,...]
        for row in 0..3u8 {
            for col in 0..8 {
                assert_eq!(out[row as usize * 8 + col], row);
            }
        }
    }

    #[test]
    fn bails_safely_on_short_buffer() {
        // Claims stride=16 height=4 (64 bytes needed) but buffer is only 20.
        // Should log a warning and pack what it can without panicking.
        let buf = vec![0xaau8; 20];
        let out = pack_frame(&buf, 4, 4, 4, Some(16));
        // Row 0 fits (16 bytes); row 1 partially overruns; loop bails.
        // We don't assert exact length — just that it didn't panic and
        // returned something <= expected.
        assert!(out.len() <= 64);
    }
}
