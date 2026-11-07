#pragma once

#include <CoreVideo/CoreVideo.h>

typedef struct CamVideo CamVideo;
typedef void (*CamFrameCallback)(void *context, CVPixelBufferRef frame);

void cam_video_initialize(void);
CamVideo *cam_video_create(void);
void cam_video_stop(CamVideo *video);
void cam_video_destroy(CamVideo *video);

// Synchronous. The callback borrows a VideoToolbox pixel buffer until it returns.
void cam_video_run(CamVideo *video, const char *url, CamFrameCallback frame,
                   void *context, char *message, size_t message_size);
