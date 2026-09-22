//! Optional Intel acceleration. A real encode probe, not encoder-name presence,
//! establishes access; unsupported inputs retry on CPU under the same reservation.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pipeline {
    Copy,
    QsvDecode,
    QsvEncode,
    Software,
}
impl Pipeline {
    pub(super) fn encoder(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::QsvDecode | Self::QsvEncode => "h264_qsv",
            Self::Software => "libx264",
        }
    }
    pub(super) fn fallback(self) -> Option<Self> {
        match self {
            Self::QsvDecode => Some(Self::QsvEncode),
            Self::QsvEncode => Some(Self::Software),
            _ => None,
        }
    }
    pub(super) fn accelerated(self) -> bool {
        matches!(self, Self::QsvDecode | Self::QsvEncode)
    }
}

pub(super) fn plan(
    copy: bool,
    available: bool,
    video: &ProbeStream,
    hdr: bool,
    interlaced: bool,
) -> Pipeline {
    if copy {
        return Pipeline::Copy;
    }
    if !available {
        return Pipeline::Software;
    }
    if !hdr
        && !interlaced
        && matches!(
            (video.codec_name.as_deref(), video.pix_fmt.as_deref()),
            (Some("h264"), Some("yuv420p")) | (Some("hevc"), Some("yuv420p" | "yuv420p10le"))
        )
        && video.width.is_some_and(|w| w > 0 && w <= 4096)
        && video.height.is_some_and(|h| h > 0 && h <= 2304)
        && video
            .sample_aspect_ratio
            .as_deref()
            .is_none_or(|sar| sar == "1:1" || sar == "N/A")
    {
        Pipeline::QsvDecode
    } else {
        Pipeline::QsvEncode
    }
}

pub(super) fn device_args(cmd: &mut Command, device: &std::path::Path) {
    cmd.arg("-init_hw_device")
        .arg(format!("qsv=viptv,child_device={}", device.display()))
        .args(["-filter_hw_device", "viptv"]);
}

pub(super) fn scale(video: &ProbeStream, width: u32, height: u32) -> String {
    let iw = video.width.unwrap_or(width).max(2);
    let ih = video.height.unwrap_or(height).max(2);
    let ratio = (width as f64 / iw as f64)
        .min(height as f64 / ih as f64)
        .min(1.0);
    let w = ((iw as f64 * ratio) as u32 / 2 * 2).max(2);
    let h = ((ih as f64 * ratio) as u32 / 2 * 2).max(2);
    // scale_qsv exposes padded green rows for cropped heights (e.g. 804) on
    // the deployed Intel/FFmpeg 5.x stack. VPP preserves the visible crop.
    format!("vpp_qsv=w={w}:h={h}:format=nv12")
}

pub(super) async fn usable(ffmpeg: &std::path::Path, device: &std::path::Path) -> bool {
    // Operator configuration only; no arbitrary FFmpeg device-expression injection.
    if !device.to_str().is_some_and(|p| {
        p.starts_with("/dev/dri/renderD") && p[16..].bytes().all(|b| b.is_ascii_digit())
    }) {
        return false;
    }
    let mut cmd = Command::new(ffmpeg);
    cmd.kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    device_args(&mut cmd, device);
    cmd.args([
        "-f",
        "lavfi",
        "-i",
        "color=size=128x128:rate=30",
        "-vf",
        "format=nv12,hwupload=extra_hw_frames=32",
        "-c:v",
        "h264_qsv",
        "-frames:v",
        "2",
        "-f",
        "null",
        "-",
    ]);
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let result = timeout(Duration::from_secs(10), child.wait()).await;
    if matches!(result, Ok(Ok(status)) if status.success()) {
        return true;
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hardware_never_overrides_copy_and_preserves_complex_filters() {
        let video: ProbeStream = serde_json::from_value(serde_json::json!({"codec_name":"hevc","pix_fmt":"yuv420p10le","width":3840,"height":2160,"sample_aspect_ratio":"1:1"})).unwrap();
        assert_eq!(plan(true, true, &video, false, false), Pipeline::Copy);
        assert_eq!(plan(false, false, &video, false, false), Pipeline::Software);
        assert_eq!(plan(false, true, &video, false, false), Pipeline::QsvDecode);
        assert_eq!(plan(false, true, &video, true, false), Pipeline::QsvEncode);
        assert_eq!(plan(false, true, &video, false, true), Pipeline::QsvEncode);
        assert_eq!(
            scale(&video, 1920, 1080),
            "vpp_qsv=w=1920:h=1080:format=nv12"
        );
        assert_eq!(
            Pipeline::QsvDecode.fallback().and_then(Pipeline::fallback),
            Some(Pipeline::Software)
        );
        assert_eq!(Pipeline::Software.fallback(), None);
    }
}
