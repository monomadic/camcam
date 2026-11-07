#include "CamVideo.h"

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/hwcontext.h>
#include <libavutil/time.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>

struct CamVideo {
    atomic_bool stopped;
    int64_t deadline;
};

void cam_video_initialize(void) {
    // Protocol diagnostics can contain credential-bearing URLs.
    av_log_set_level(AV_LOG_QUIET);
    avformat_network_init();
}

CamVideo *cam_video_create(void) {
    CamVideo *video = calloc(1, sizeof(*video));
    if (video) atomic_init(&video->stopped, false);
    return video;
}

void cam_video_stop(CamVideo *video) {
    atomic_store(&video->stopped, true);
}

void cam_video_destroy(CamVideo *video) {
    free(video);
}

static int interrupted(void *opaque) {
    CamVideo *video = opaque;
    return atomic_load(&video->stopped) || av_gettime_relative() >= video->deadline;
}

static enum AVPixelFormat hardware_format(AVCodecContext *decoder,
                                           const enum AVPixelFormat *formats) {
    for (; *formats != AV_PIX_FMT_NONE; formats++) {
        if (*formats == AV_PIX_FMT_VIDEOTOOLBOX) return *formats;
    }
    return AV_PIX_FMT_NONE;
}

void cam_video_run(CamVideo *video, const char *url, CamFrameCallback frame,
                   void *context, char *message, size_t message_size) {
    AVFormatContext *input = avformat_alloc_context();
    AVCodecContext *decoder = NULL;
    AVPacket *packet = NULL;
    AVFrame *decoded = NULL;
    AVDictionary *options = NULL;
    const AVCodec *codec = NULL;
    const char *stage = "Allocate video session";
    int result = AVERROR(ENOMEM);
    if (!input) goto done;

    video->deadline = av_gettime_relative() + 12000000;
    input->interrupt_callback = (AVIOInterruptCB){interrupted, video};
    av_dict_set(&options, "protocol_whitelist", "rtsp,rtmp,rtmps,tcp,tls,udp,rtp,http,https,httpproxy", 0);
    av_dict_set(&options, "rtsp_transport", "tcp", 0);
    av_dict_set(&options, "analyzeduration", "500000", 0);
    av_dict_set(&options, "probesize", "262144", 0);
    av_dict_set(&options, "rtmp_live", "live", 0);
    av_dict_set(&options, "rtmp_buffer", "300", 0);

    stage = "Open stream";
    result = avformat_open_input(&input, url, NULL, &options);
    if (result < 0) goto done;

    stage = "Read stream information";
    result = avformat_find_stream_info(input, NULL);
    if (result < 0) goto done;
    result = av_find_best_stream(input, AVMEDIA_TYPE_VIDEO, -1, -1, &codec, 0);
    if (result < 0) goto done;
    int stream = result;

    stage = "Select hardware decoder";
    result = AVERROR(ENOSYS);
    if (codec->id != AV_CODEC_ID_H264 && codec->id != AV_CODEC_ID_HEVC) goto done;

    stage = "Allocate decoder";
    result = AVERROR(ENOMEM);
    decoder = avcodec_alloc_context3(codec);
    packet = av_packet_alloc();
    decoded = av_frame_alloc();
    if (!decoder || !packet || !decoded) goto done;

    stage = "Configure VideoToolbox";
    result = avcodec_parameters_to_context(decoder, input->streams[stream]->codecpar);
    if (result < 0) goto done;
    decoder->get_format = hardware_format;
    decoder->thread_count = 1;
    result = av_hwdevice_ctx_create(&decoder->hw_device_ctx, AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
                                    NULL, NULL, 0);
    if (result < 0) goto done;
    result = avcodec_open2(decoder, codec, NULL);
    if (result < 0) goto done;

    bool started = false;
    while (!interrupted(video)) {
        stage = "Read video packets";
        result = av_read_frame(input, packet);
        if (result < 0) goto done;
        if (packet->stream_index != stream) {
            av_packet_unref(packet);
            continue;
        }

        // Joining a live stream mid-GOP can make VideoToolbox reject the first
        // inter-frame. Wait for a random-access packet before starting decoding.
        if (!started && !(packet->flags & AV_PKT_FLAG_KEY)) {
            av_packet_unref(packet);
            continue;
        }
        started = true;

        stage = "Decode with VideoToolbox";
        result = avcodec_send_packet(decoder, packet);
        av_packet_unref(packet);
        if (result < 0) goto done;
        while ((result = avcodec_receive_frame(decoder, decoded)) >= 0) {
            if (decoded->format != AV_PIX_FMT_VIDEOTOOLBOX || !decoded->data[3]) {
                result = AVERROR(ENOSYS);
                goto done;
            }
            video->deadline = av_gettime_relative() + 8000000;
            frame(context, (CVPixelBufferRef)decoded->data[3]);
            av_frame_unref(decoded);
        }
        if (result != AVERROR(EAGAIN)) goto done;
    }

done:
    if (!atomic_load(&video->stopped)) {
        char reason[AV_ERROR_MAX_STRING_SIZE];
        if (interrupted(video)) {
            snprintf(message, message_size, "%s: timed out", stage);
        } else {
            av_strerror(result, reason, sizeof(reason));
            snprintf(message, message_size, "%s: %s", stage, reason);
        }
    }
    av_dict_free(&options);
    av_frame_free(&decoded);
    av_packet_free(&packet);
    avcodec_free_context(&decoder);
    avformat_close_input(&input);
}
