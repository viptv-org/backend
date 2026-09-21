use super::*;

impl PlaybackManager {
    #[allow(clippy::too_many_arguments)]
    pub async fn start_with_selection(
        &self,
        url: String,
        headers: HashMap<String, String>,
        position: f64,
        capabilities: Option<Capabilities>,
        force: bool,
        live: bool,
        provider_permit: Option<OwnedSemaphorePermit>,
        selection: TrackSelection,
    ) -> Result<PlaybackResponse, String> {
        if selection
            .audio_track_index
            .is_some_and(|index| index > 65535)
        {
            return Err("Requested input audio track index is out of range".into());
        }
        if selection
            .subtitle_track_index
            .is_some_and(|index| index > 65535)
        {
            return Err("Requested input subtitle track index is out of range".into());
        }
        if live && position != 0.0 {
            return Err("Live playback does not support offset seeking".into());
        }
        let _lifecycle = self.lifecycle.read().await;
        if self.slots.is_closed() {
            return Err("Playback is shutting down".into());
        }
        // Never propagate parser/provider/subprocess errors: they may contain credentials.
        let validated =
            crate::util::validate_url(&url).map_err(|_| "Invalid playback URL".to_owned())?;
        if !matches!(validated.scheme(), "http" | "https")
            || !validated.username().is_empty()
            || validated.password().is_some()
            || validated.fragment().is_some()
        {
            return Err("Invalid playback URL".into());
        }
        if !position.is_finite() || !(0.0..=604800.0).contains(&position) {
            return Err("Invalid playback position".into());
        }
        let header_block = header_block(&headers)?;
        let caps = capabilities.unwrap_or_default();
        let (width, height) = dimensions(&caps)?;
        if !caps.h264 || !caps.aac {
            return Err("H264 and AAC playback support is required".into());
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            // Distinct from a provider-connection limit: this is the server's own
            // session budget (VIPTV_MAX_SESSIONS), which needs the viewer to stop
            // something rather than to retry. It is reported as 503, not 429.
            .map_err(|_| MSG_PLAYBACK_CAPACITY.to_owned())?;
        let permits = Arc::new(InputPermits {
            _playback: permit,
            _provider: provider_permit,
        });
        self.initialize().await?;
        let probe_started = Instant::now();
        let probe = self
            .cached_probe(
                validated.as_str(),
                &header_block,
                live,
                Some(permits.clone()),
            )
            .await
            .ok_or_else(|| {
                tracing::warn!(live, "Playback source inspection failed");
                "Could not inspect source video safely; try another stream".to_owned()
            })?;
        let probe_ms = probe_started.elapsed().as_millis() as u64;
        // The capability envelope gates only the managed/transcode path. Original
        // delivery is decided below from the client's declared decoders, so an
        // HDR, wide-gamut or otherwise unusual source is still playable whenever
        // the client can demux and decode it.
        let selected_input = probe.select_audio(&selection)?;
        let caption_index = selection.subtitle_track_index.or_else(|| {
            let language = selection.preferred_subtitle_language.as_deref()?;
            probe
                .streams
                .iter()
                .filter(|s| s.codec_type.as_deref() == Some("subtitle") && s.text_subtitle())
                .filter(|s| {
                    s.language().is_some_and(|actual| {
                        normalize_audio_language(&actual) == normalize_audio_language(language)
                    })
                })
                .take(32)
                .find_map(|s| s.index.filter(|i| *i <= 65535))
        });
        let selected_caption = probe.select_subtitle(caption_index)?;
        let mut audio_tracks = probe.tracks("audio");
        let mut subtitle_tracks = probe.tracks("subtitle");
        for track in audio_tracks.iter_mut().chain(subtitle_tracks.iter_mut()) {
            if let Some(stream) = probe
                .streams
                .iter()
                .find(|s| s.index == Some(track.input_index))
            {
                if let Some(title) = stream
                    .tags
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case("title"))
                    .map(|(_, value)| value)
                {
                    let title =
                        crate::source_display_text(title, 128, validated.as_str(), &headers);
                    if !title.is_empty() {
                        track.title = title;
                    }
                }
            }
            track.selected = selected_input.and_then(|s| s.index) == Some(track.input_index)
                || selected_caption.and_then(|s| s.index) == Some(track.input_index);
        }
        let selected_audio = selected_input.map(|audio| SelectedAudio {
            input_index: audio.index.expect("selection validates stream index"),
            output_index: 0,
            output_audio_ordinal: 0,
            output_stream_index: 1,
            language: audio.language(),
            language_status: audio.language_status().into(),
            disposition: audio.disposition.as_ref().map(ProbeDisposition::public),
            title: audio_tracks
                .iter()
                .find(|t| t.selected)
                .map(|t| t.title.clone())
                .unwrap_or_default(),
        });
        let selected_subtitle = selected_caption.map(|caption| SelectedSubtitle {
            input_index: caption.index.expect("selection validates stream index"),
            output_index: 0,
            output_stream_index: if selected_audio.is_some() { 2 } else { 1 },
            language: caption.language(),
            language_status: caption.language_status().into(),
            disposition: caption.disposition.as_ref().map(ProbeDisposition::public),
            title: subtitle_tracks
                .iter()
                .find(|t| t.selected)
                .map(|t| t.title.clone())
                .unwrap_or_default(),
        });
        if !live
            && probe
                .duration()
                .is_some_and(|duration| position >= duration)
        {
            return Err("Playback position is past the end of this source".into());
        }
        // A native client that fetches sources itself receives the original URL
        // with the server's upstream authorization. Its own decoders decide
        // playability: this server never proxies, transcodes, or applies codec
        // policy for such a client.
        if !force && caps.direct_urls == Some(true) {
            let format = if live {
                "hls"
            } else {
                probe
                    .format
                    .get("format_name")
                    .and_then(|name| name.as_str())
                    .and_then(direct_file_extension)
                    .unwrap_or("file")
            };
            let authorization = source_authorization(&headers);
            let id = Uuid::new_v4().to_string();
            let capability = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
            let response = PlaybackResponse {
                id: id.clone(),
                url: validated.as_str().to_owned(),
                format: format.into(),
                mode: "direct".into(),
                video_mode: "copy".into(),
                audio_mode: "copy".into(),
                position,
                live,
                duration: if live {
                    0.0
                } else {
                    probe.duration().unwrap_or(0.0)
                },
                audio_tracks,
                subtitles_supported: subtitle_tracks.iter().any(|track| track.supported),
                subtitle_tracks,
                selected_audio,
                selected_subtitle,
                authorization,
            };
            self.sessions.lock().await.insert(
                id,
                Session {
                    _source_probe: (!live).then(|| probe.clone()),
                    direct: None,
                    capability,
                    dir: PathBuf::new(),
                    child: None,
                    touched: Instant::now(),
                    stable_target_duration: false,
                    supervised_live: false,
                    permits,
                    cleanup_tasks: self.cleanup_tasks.clone(),
                },
            );
            tracing::info!(
                mode = "direct",
                format,
                probe_ms,
                "Playback preparation completed"
            );
            return Ok(response);
        }

        // Original delivery is opt-in, uses inspected tracks, and keeps the
        // provider reservation owned by this session and in-flight reads.
        if !force && caps.direct_play && std::env::var("VIPTV_DIRECT_PLAY").as_deref() != Ok("0") {
            if let Some(format) = direct_format(&probe, &caps, selected_input, &selection) {
                if let Ok(transport) =
                    direct::Direct::prepare(validated.clone(), &headers, format, permits.clone())
                        .await
                {
                    let id = Uuid::new_v4().to_string();
                    let capability =
                        format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
                    let filename = if format == "hls" {
                        "index.m3u8"
                    } else {
                        // Original files are served under their own container
                        // name; the client demuxes them itself.
                        Box::leak(format!("source.{format}").into_boxed_str())
                    };
                    // A native file can only retain one default audio track here;
                    // arbitrary track changes deliberately return to managed HLS.
                    let response = PlaybackResponse {
                        id: id.clone(),
                        url: format!("/media/{id}/{capability}/{filename}"),
                        format: format.into(),
                        mode: "direct".into(),
                        video_mode: "copy".into(),
                        audio_mode: if selected_input.is_some() {
                            "copy"
                        } else {
                            "none"
                        }
                        .into(),
                        position,
                        live,
                        duration: if live {
                            0.0
                        } else {
                            probe.duration().unwrap_or(0.0)
                        },
                        audio_tracks,
                        subtitles_supported: subtitle_tracks.iter().any(|track| track.supported),
                        subtitle_tracks,
                        selected_audio,
                        selected_subtitle: None,
                        authorization: None,
                    };
                    self.sessions.lock().await.insert(
                        id,
                        Session {
                            _source_probe: (!live).then(|| probe.clone()),
                            direct: Some(transport),
                            capability,
                            dir: PathBuf::new(),
                            child: None,
                            touched: Instant::now(),
                            stable_target_duration: false,
                            supervised_live: false,
                            permits,
                            cleanup_tasks: self.cleanup_tasks.clone(),
                        },
                    );
                    tracing::info!(
                        mode = "direct",
                        format,
                        probe_ms,
                        "Playback preparation completed"
                    );
                    return Ok(response);
                }
                tracing::info!(
                    reason = "original_transport_unavailable",
                    "Using managed playback fallback"
                );
            }
        }
        // Managed output re-encodes into the envelope the browser declared, so the
        // inspected source must be inside it. Original delivery above is exempt:
        // there the client's own decoders, not this policy, decide.
        probe.ensure_supported()?;
        let hdr = probe.hdr_transfer()?.is_some();
        let interlaced = probe.interlaced();
        let mut transforms = Vec::new();
        if interlaced || hdr {
            let filters = self.available_filters().await?;
            if interlaced {
                let filter = if filters.contains("bwdif") {
                    "bwdif"
                } else if filters.contains("yadif") {
                    "yadif"
                } else {
                    return Err(
                        "Interlaced playback requires the bwdif or yadif FFmpeg filter".into(),
                    );
                };
                transforms.push(format!(
                    "{filter}=mode=send_frame:parity=auto:deint=all,setfield=prog"
                ));
            }
            if hdr {
                if !["zscale", "tonemap", "sidedata"]
                    .iter()
                    .all(|name| filters.contains(*name))
                {
                    return Err("HDR10/HLG conversion requires FFmpeg zscale, tonemap and sidedata filters; select an SDR source".into());
                }
                transforms.push(hdr_filter(width, height));
            }
        }
        if !hdr {
            transforms.push(scale_filter(width, height));
        }
        let duration = if live { None } else { probe.duration() };
        if duration.is_some_and(|duration| position >= duration) {
            return Err("Playback position is past the end of this source".into());
        }
        // Input-side -ss plus stream copy can start on an earlier keyframe or
        // discard reference frames. Only decoding provides accurate arbitrary seeks.
        // At offset zero, copy compatible H264 video even when audio alone needs AAC
        // conversion. This avoids wasting CPU re-encoding already-compatible pictures.
        let copy_video =
            !force && position == 0.0 && probe.compatible_video(width, height, H264_COPY_LEVEL);
        // Copy audio independently at zero offset. After input-side seeking,
        // decoded audio is required to keep MPEGTS/WebVTT on the same clock.
        let copy_audio = !force && position == 0.0 && probe.compatible_audio_stream(selected_input);
        let remux = copy_video && copy_audio;
        let id = Uuid::new_v4().to_string();
        let capability = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        // An absolute output path keeps relative config roots independent of FFmpeg's cwd.
        tokio::fs::create_dir_all(&self.config.root)
            .await
            .map_err(|_| "Media storage unavailable".to_owned())?;
        let root = tokio::fs::canonicalize(&self.config.root)
            .await
            .map_err(|_| "Media storage unavailable".to_owned())?;
        let dir = root.join(&id);
        let mut session = Session {
            _source_probe: (!live).then(|| probe.clone()),
            direct: None,
            capability: capability.clone(),
            dir: dir.clone(),
            child: None,
            touched: Instant::now(),
            stable_target_duration: true,
            supervised_live: false,
            permits,
            cleanup_tasks: self.cleanup_tasks.clone(),
        };
        tokio::fs::create_dir(&dir)
            .await
            .map_err(|_| "Media storage unavailable".to_owned())?;
        let mut pipeline = hardware::plan(
            copy_video,
            !copy_video && self.qsv_available().await,
            probe.video()?,
            hdr,
            interlaced,
        );
        let engine_started = Instant::now();
        loop {
            let mut cmd = Command::new(&self.config.ffmpeg);
            cmd.kill_on_drop(true)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
            if pipeline.accelerated() {
                hardware::device_args(
                    &mut cmd,
                    self.qsv_device.as_ref().expect("validated device"),
                );
                if pipeline == hardware::Pipeline::QsvDecode {
                    cmd.args(["-hwaccel", "qsv", "-hwaccel_output_format", "qsv"]);
                    // FFmpeg 5.x needs the explicit QSV decoder to keep frames on GPU.
                    let decoder = if probe.video()?.codec_name.as_deref() == Some("hevc") {
                        "hevc_qsv"
                    } else {
                        "h264_qsv"
                    };
                    cmd.args(["-c:v", decoder]);
                }
            }
            input_args(&mut cmd, &header_block);
            // Live inputs must stay realtime; VOD can fill the rolling window as fast
            // as FFmpeg can produce it so browser playback has actual headroom.
            if live {
                cmd.arg("-re");
            }
            if position > 0.0 {
                cmd.arg("-ss").arg(format!("{position:.3}"));
            }
            cmd.arg("-i").arg(validated.as_str());
            cmd.args(["-map", "0:v:0"]);
            if let Some(audio) = &selected_audio {
                cmd.arg("-map").arg(format!("0:{}", audio.input_index));
            }
            if let Some(subtitle) = &selected_subtitle {
                cmd.arg("-map").arg(format!("0:{}", subtitle.input_index));
                cmd.args(["-c:s", "webvtt", "-max_interleave_delta", "1000000"]);
            } else {
                cmd.arg("-sn");
            }
            cmd.args(["-dn", "-map_metadata", "-1", "-map_chapters", "-1"]);
            if let Some(audio) = &selected_audio {
                cmd.arg("-metadata:s:a:0")
                    .arg(if audio.language_status == "tagged_english" {
                        "language=eng"
                    } else {
                        "language=und"
                    });
            }
            if copy_video {
                cmd.args(["-c:v", "copy"]);
            } else {
                let gop = (HLS_SEGMENT_SECONDS * 30).to_string();
                let force_key_frames = format!(
                "expr:gte(t,if(eq(n_forced,0),0,{HLS_INITIAL_SEGMENT_SECONDS}+(n_forced-1)*{HLS_SEGMENT_SECONDS}))"
            );
                let filter = match pipeline {
                    hardware::Pipeline::QsvDecode => hardware::scale(probe.video()?, width, height),
                    hardware::Pipeline::QsvEncode => format!(
                        "{},format=nv12,hwupload=extra_hw_frames=64",
                        transforms.join(",")
                    ),
                    _ => transforms.join(","),
                };
                cmd.arg("-vf").arg(filter);
                if hdr {
                    cmd.args([
                        "-color_primaries",
                        "bt709",
                        "-color_trc",
                        "bt709",
                        "-colorspace",
                        "bt709",
                        "-color_range",
                        "tv",
                    ]);
                }
                if pipeline.accelerated() {
                    cmd.args([
                        "-c:v",
                        "h264_qsv",
                        "-preset",
                        "veryfast",
                        "-profile:v",
                        "main",
                        "-level:v",
                        "4.0",
                        "-b:v",
                        "4000k",
                        "-maxrate",
                        "5000k",
                        "-bufsize",
                        "10000k",
                        "-look_ahead",
                        "0",
                        "-async_depth",
                        "1",
                        "-bf",
                        "0",
                        "-fpsmax",
                        "30",
                        "-g",
                        &gop,
                        "-forced_idr",
                        "1",
                        "-force_key_frames",
                        &force_key_frames,
                    ]);
                } else {
                    cmd.args([
                        "-c:v",
                        "libx264",
                        "-preset",
                        "ultrafast",
                        "-tune",
                        "zerolatency",
                        "-profile:v",
                        "main",
                        "-level:v",
                        "4.0",
                        "-pix_fmt",
                        "yuv420p",
                        "-crf",
                        "23",
                        "-maxrate",
                        "5000k",
                        "-bufsize",
                        "10000k",
                        "-fpsmax",
                        "30",
                        "-g",
                        &gop,
                        "-keyint_min",
                        &gop,
                        "-sc_threshold",
                        "0",
                        "-flags",
                        "+cgop",
                        "-x264-params",
                        "open-gop=0",
                        "-forced-idr",
                        "1",
                        "-force_key_frames",
                        &force_key_frames,
                    ]);
                }
            }
            if selected_input.is_some() {
                if copy_audio {
                    cmd.args(["-c:a", "copy"]);
                } else {
                    cmd.args(["-c:a", "aac", "-b:a", "128k", "-ac", "2", "-ar", "48000"]);
                }
            }
            if let Some(subtitle) = &selected_subtitle {
                let language = subtitle.language.as_deref().unwrap_or("und");
                let audio_map = if selected_audio.is_some() { "a:0," } else { "" };
                // Keep AV and WebVTT on the same post-seek timestamp clock. Without
                // copyts, the nested MPEGTS muxer adds a private offset absent from VTT.
                cmd.args(["-hls_segment_options", "mpegts_copyts=1", "-var_stream_map"])
                    .arg(format!(
                        "v:0,{audio_map}s:0,sgroup:subs,language:{language}"
                    ));
            }
            let initial_segment_seconds = HLS_INITIAL_SEGMENT_SECONDS.to_string();
            let segment_seconds = HLS_SEGMENT_SECONDS.to_string();
            let list_size = (HLS_WINDOW_SECONDS / HLS_SEGMENT_SECONDS).to_string();
            let delete_threshold = (HLS_DELETE_GRACE_SECONDS / HLS_SEGMENT_SECONDS).to_string();
            cmd.args([
                "-max_muxing_queue_size",
                "1024",
                "-f",
                "hls",
                "-hls_init_time",
                &initial_segment_seconds,
            ]);
            let hls_flags = if copy_video {
                "delete_segments+temp_file+split_by_time"
            } else {
                "delete_segments+temp_file"
            };
            cmd.args([
                "-hls_time",
                &segment_seconds,
                "-hls_list_size",
                &list_size,
                "-hls_delete_threshold",
                &delete_threshold,
                "-hls_flags",
                hls_flags,
                "-hls_segment_filename",
            ]);
            cmd.arg(dir.join("segment-%09d.ts"))
                .arg(dir.join("index.m3u8"));
            session.child = Some(
                cmd.spawn()
                    .map_err(|_| "Playback engine unavailable".to_owned())?,
            );
            // Do not return a URL until both the playlist and first segment exist.
            let ready = timeout(Duration::from_secs(if pipeline.accelerated() { 12 } else { 30 }), async {
            loop {
                if !cache_safe(&dir, false).await {
                    return false;
                }
                if let Some(child) = session.child.as_mut() {
                    if let Ok(Some(status)) = child.try_wait() {
                        if !status.success() {
                            tracing::warn!(exit_code = ?status.code(), "Playback engine startup failed");
                            return false;
                        }
                        return playback_ready(&dir, selected_subtitle.as_ref()).await;
                    }
                }
                if playback_ready(&dir, selected_subtitle.as_ref()).await {
                    return true;
                }
                sleep(Duration::from_millis(150)).await;
            }
        })
        .await
        .unwrap_or(false);
            if ready {
                break;
            }
            if let Some(next) = pipeline.fallback() {
                if let Some(mut child) = session.child.take() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                // A failed attempt may have emitted a partial playlist. Never reuse it.
                tokio::fs::remove_dir_all(&dir)
                    .await
                    .map_err(|_| "Playback cleanup failed")?;
                tokio::fs::create_dir(&dir)
                    .await
                    .map_err(|_| "Playback storage unavailable")?;
                tracing::warn!(from = ?pipeline, to = ?next, "Retrying playback pipeline");
                pipeline = next;
                continue;
            }
            if !ready {
                session.cleanup().await;
                return Err(
                    "Playback could not start; try forced transcoding or another stream".into(),
                );
            }
        }
        let engine_ready_ms = engine_started.elapsed().as_millis() as u64;
        tracing::info!(
            probe_ms,
            engine_ready_ms,
            encoder = pipeline.encoder(),
            pipeline = ?pipeline,
            live,
            video_mode = if copy_video { "copy" } else { "encode" },
            audio_mode = if selected_input.is_none() {
                "none"
            } else if copy_audio {
                "copy"
            } else {
                "encode"
            },
            "Playback preparation completed"
        );
        session.touched = Instant::now();
        self.sessions.lock().await.insert(id.clone(), session);
        Ok(PlaybackResponse {
            url: format!(
                "/media/{id}/{capability}/{}",
                if selected_subtitle.is_some() {
                    "master.m3u8"
                } else {
                    "index.m3u8"
                }
            ),
            id,
            format: "hls".into(),
            mode: if remux { "remux" } else { "transcode" }.into(),
            video_mode: if copy_video { "copy" } else { "encode" }.into(),
            audio_mode: if selected_input.is_none() {
                "none"
            } else if copy_audio {
                "copy"
            } else {
                "encode"
            }
            .into(),
            position,
            live,
            duration: duration.unwrap_or(0.0),
            audio_tracks,
            subtitles_supported: subtitle_tracks.iter().any(|track| track.supported),
            subtitle_tracks,
            selected_audio,
            selected_subtitle,
            authorization: None,
        })
    }
}
