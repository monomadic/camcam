//! Live FFmpeg demux/decode with VideoToolbox output. Connections stay on their worker.
use anyhow::{Result, bail, ensure};
use ffmpeg_sys_next as av;
use std::{
    ffi::{CString, c_int, c_void},
    ptr,
    sync::{
        Arc, Once,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferRetain(buffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferRelease(buffer: *mut c_void);
    fn CVPixelBufferGetWidth(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetHeight(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetPixelFormatType(buffer: *mut c_void) -> u32;
}

/// A retained, immutable VideoToolbox pixel buffer. GPU imports retain it until GPU work completes.
pub struct VideoFrame {
    buffer: *mut c_void,
    pub width: u32,
    pub height: u32,
    pub ten_bit: bool,
    pub full_range: bool,
    pub matrix: Matrix,
}

// VideoToolbox has finished writing this buffer before it is published. All consumers read it;
// the retained CVPixelBuffer owns its backing IOSurface across threads.
unsafe impl Send for VideoFrame {}
unsafe impl Sync for VideoFrame {}

#[derive(Clone, Copy, Debug)]
pub enum Matrix {
    Bt601,
    Bt709,
    Bt2020,
}

impl VideoFrame {
    pub fn buffer(&self) -> *mut c_void {
        self.buffer
    }

    unsafe fn from_decoded(frame: &av::AVFrame) -> Result<Self> {
        ensure!(
            frame.format == av::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32,
            "Decoder did not produce a VideoToolbox frame"
        );
        let buffer = frame.data[3].cast::<c_void>();
        ensure!(!buffer.is_null(), "Decoder returned an empty pixel buffer");
        // SAFETY: VideoToolbox frame.data[3] is borrowed until av_frame_unref.
        let format = unsafe { CVPixelBufferGetPixelFormatType(buffer) };
        let ten_bit = match format.to_be_bytes() {
            [b'4', b'2', b'0', b'v'] | [b'4', b'2', b'0', b'f'] => false,
            [b'x', b'4', b'2', b'0'] | [b'x', b'f', b'2', b'0'] => true,
            _ => bail!("Unsupported VideoToolbox pixel format"),
        };
        let full_range = matches!(
            format.to_be_bytes(),
            [b'4', b'2', b'0', b'f'] | [b'x', b'f', b'2', b'0']
        );
        let matrix = match frame.colorspace {
            av::AVColorSpace::AVCOL_SPC_BT2020_NCL | av::AVColorSpace::AVCOL_SPC_BT2020_CL => {
                Matrix::Bt2020
            }
            av::AVColorSpace::AVCOL_SPC_BT709 => Matrix::Bt709,
            av::AVColorSpace::AVCOL_SPC_UNSPECIFIED
                if unsafe { CVPixelBufferGetHeight(buffer) } >= 720 =>
            {
                Matrix::Bt709
            }
            _ => Matrix::Bt601,
        };
        Ok(Self {
            buffer: unsafe { CVPixelBufferRetain(buffer) },
            width: unsafe { CVPixelBufferGetWidth(buffer) } as u32,
            height: unsafe { CVPixelBufferGetHeight(buffer) } as u32,
            ten_bit,
            full_range,
            matrix,
        })
    }
}

impl Drop for VideoFrame {
    fn drop(&mut self) {
        // SAFETY: exactly balances the retain in from_decoded.
        unsafe { CVPixelBufferRelease(self.buffer) }
    }
}

struct Interrupt {
    stop: Arc<AtomicBool>,
    deadline: Instant,
}

unsafe extern "C" fn interrupted(context: *mut c_void) -> c_int {
    // SAFETY: a boxed Interrupt lives until the format context has been closed.
    let interrupt = unsafe { &*context.cast::<Interrupt>() };
    (interrupt.stop.load(Ordering::Relaxed) || Instant::now() >= interrupt.deadline) as c_int
}

unsafe extern "C" fn hardware_format(
    _: *mut av::AVCodecContext,
    mut formats: *const av::AVPixelFormat,
) -> av::AVPixelFormat {
    // SAFETY: FFmpeg supplies an AV_PIX_FMT_NONE-terminated array.
    unsafe {
        while *formats != av::AVPixelFormat::AV_PIX_FMT_NONE {
            if *formats == av::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX {
                return *formats;
            }
            formats = formats.add(1);
        }
    }
    av::AVPixelFormat::AV_PIX_FMT_NONE
}

/// Frees all partially initialized FFmpeg objects, including on errors and cancellation.
struct Decoder {
    input: *mut av::AVFormatContext,
    codec: *mut av::AVCodecContext,
    packet: *mut av::AVPacket,
    frame: *mut av::AVFrame,
    options: *mut av::AVDictionary,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: these pointers are either null or exclusively owned FFmpeg allocations.
        unsafe {
            av::av_dict_free(&mut self.options);
            av::av_frame_free(&mut self.frame);
            av::av_packet_free(&mut self.packet);
            av::avcodec_free_context(&mut self.codec);
            av::avformat_close_input(&mut self.input);
        }
    }
}

fn check(code: i32, stage: &str, interrupt: &Interrupt) -> Result<()> {
    if code >= 0 {
        return Ok(());
    }
    if interrupt.stop.load(Ordering::Relaxed) {
        bail!("Stopped");
    }
    if Instant::now() >= interrupt.deadline {
        bail!("{stage}: timed out");
    }
    let mut message = [0i8; 128];
    // SAFETY: writable bounded buffer. av_strerror contains no credential-bearing stream URL.
    let reason = unsafe {
        av::av_strerror(code, message.as_mut_ptr(), message.len());
        std::ffi::CStr::from_ptr(message.as_ptr()).to_string_lossy()
    };
    bail!("{stage}: {reason}")
}

/// Callbacks receive owned pixel buffers. Return false to stop after the current frame.
pub fn decode(
    url: &Url,
    stop: Arc<AtomicBool>,
    mut receive: impl FnMut(Arc<VideoFrame>) -> bool,
) -> Result<()> {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        // FFmpeg protocol logs can contain credentials.
        av::av_log_set_level(av::AV_LOG_QUIET);
        av::avformat_network_init();
    });
    let url = CString::new(url.as_str()).map_err(|_| anyhow::anyhow!("Invalid stream URL"))?;
    let mut interrupt = Box::new(Interrupt {
        stop,
        deadline: Instant::now() + Duration::from_secs(12),
    });
    // Declared after interrupt so the format context is closed before its callback storage drops.
    let mut decoder = Decoder {
        input: unsafe { av::avformat_alloc_context() },
        codec: ptr::null_mut(),
        packet: ptr::null_mut(),
        frame: ptr::null_mut(),
        options: ptr::null_mut(),
    };
    ensure!(!decoder.input.is_null(), "Could not allocate video session");
    // SAFETY: every FFmpeg context below is exclusively owned by this worker. The interrupt
    // pointer remains stable in its Box. CVPixelBuffers are retained before AVFrames are freed.
    unsafe {
        (*decoder.input).interrupt_callback = av::AVIOInterruptCB {
            callback: Some(interrupted),
            opaque: (&mut *interrupt as *mut Interrupt).cast(),
        };
        for (key, value) in [
            (
                "protocol_whitelist",
                "rtsp,rtmp,rtmps,tcp,tls,udp,rtp,http,https,httpproxy",
            ),
            ("rtsp_transport", "tcp"),
            ("analyzeduration", "500000"),
            ("probesize", "262144"),
            ("rtmp_live", "live"),
            ("rtmp_buffer", "300"),
        ] {
            av::av_dict_set(
                &mut decoder.options,
                CString::new(key)?.as_ptr(),
                CString::new(value)?.as_ptr(),
                0,
            );
        }
        check(
            av::avformat_open_input(
                &mut decoder.input,
                url.as_ptr(),
                ptr::null(),
                &mut decoder.options,
            ),
            "Open stream",
            &interrupt,
        )?;
        check(
            av::avformat_find_stream_info(decoder.input, ptr::null_mut()),
            "Read stream information",
            &interrupt,
        )?;
        let mut codec = ptr::null();
        let stream = av::av_find_best_stream(
            decoder.input,
            av::AVMediaType::AVMEDIA_TYPE_VIDEO,
            -1,
            -1,
            &mut codec,
            0,
        );
        check(stream, "Find video stream", &interrupt)?;
        ensure!(!codec.is_null(), "Stream has no video decoder");
        ensure!(
            matches!(
                (*codec).id,
                av::AVCodecID::AV_CODEC_ID_H264 | av::AVCodecID::AV_CODEC_ID_HEVC
            ),
            "Hardware playback requires H.264 or HEVC"
        );
        decoder.codec = av::avcodec_alloc_context3(codec);
        decoder.packet = av::av_packet_alloc();
        decoder.frame = av::av_frame_alloc();
        ensure!(
            !decoder.codec.is_null() && !decoder.packet.is_null() && !decoder.frame.is_null(),
            "Could not allocate video decoder"
        );
        let parameters = (*(*(*decoder.input).streams.add(stream as usize))).codecpar;
        check(
            av::avcodec_parameters_to_context(decoder.codec, parameters),
            "Configure decoder",
            &interrupt,
        )?;
        (*decoder.codec).get_format = Some(hardware_format);
        (*decoder.codec).thread_count = 1;
        check(
            av::av_hwdevice_ctx_create(
                &mut (*decoder.codec).hw_device_ctx,
                av::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
                ptr::null(),
                ptr::null_mut(),
                0,
            ),
            "Configure VideoToolbox",
            &interrupt,
        )?;
        check(
            av::avcodec_open2(decoder.codec, codec, ptr::null_mut()),
            "Open VideoToolbox decoder",
            &interrupt,
        )?;

        let mut started = false;
        loop {
            if interrupt.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            check(
                av::av_read_frame(decoder.input, decoder.packet),
                "Read video packets",
                &interrupt,
            )?;
            let packet = &*decoder.packet;
            if packet.stream_index != stream
                || (!started && packet.flags & av::AV_PKT_FLAG_KEY == 0)
            {
                av::av_packet_unref(decoder.packet);
                continue;
            }
            // VideoToolbox cannot reliably start from the middle of a live GOP.
            started = true;
            let result = av::avcodec_send_packet(decoder.codec, decoder.packet);
            av::av_packet_unref(decoder.packet);
            check(result, "Decode with VideoToolbox", &interrupt)?;
            loop {
                let result = av::avcodec_receive_frame(decoder.codec, decoder.frame);
                if result == av::AVERROR(libc::EAGAIN) {
                    break;
                }
                check(result, "Decode with VideoToolbox", &interrupt)?;
                let frame = Arc::new(VideoFrame::from_decoded(&*decoder.frame)?);
                av::av_frame_unref(decoder.frame);
                interrupt.deadline = Instant::now() + Duration::from_secs(8);
                if !receive(frame) {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires scripts/test-live.sh fixtures and macOS video hardware"]
    fn repeated_live_joins_use_videotoolbox() {
        for (url, width) in [
            ("rtmp://127.0.0.1:11935/workshop", 640),
            ("rtsp://127.0.0.1:18554/main", 1280),
            ("rtsp://127.0.0.1:18554/sub", 480),
            ("rtsp://127.0.0.1:18554/hevc10", 480),
        ] {
            for _ in 0..3 {
                let start = Instant::now();
                let mut frames = 0;
                decode(
                    &Url::parse(url).unwrap(),
                    Arc::new(AtomicBool::new(false)),
                    |frame| {
                        assert_eq!(frame.width, width);
                        frames += 1;
                        if frames == 1 {
                            eprintln!("First VideoToolbox frame {url}: {:?}", start.elapsed());
                        }
                        frames < 20
                    },
                )
                .unwrap();
                assert_eq!(frames, 20);
            }
        }
    }
}
