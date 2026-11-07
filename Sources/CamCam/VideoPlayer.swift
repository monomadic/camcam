import AVFoundation
import CCamVideo
import Foundation

// FFmpeg owns the connection on a worker. Keep at most one frame waiting for the UI.
final class VideoPlayer: @unchecked Sendable {
    private let session: OpaquePointer
    private let mailbox = NSLock()
    private var pendingFrame: CVPixelBuffer?
    private var deliveryScheduled = false
    @MainActor private var onFrame: ((CVPixelBuffer) -> Void)?
    @MainActor private var onEnd: ((String) -> Void)?

    init?() {
        guard let session = cam_video_create() else { return nil }
        self.session = session
    }

    deinit { cam_video_destroy(session) }

    @MainActor
    func start(_ url: URL, onFrame: @escaping (CVPixelBuffer) -> Void, onEnd: @escaping (String) -> Void) {
        self.onFrame = onFrame
        self.onEnd = onEnd
        DispatchQueue.global(qos: .userInitiated).async { [self] in
            var message = [CChar](repeating: 0, count: 256)
            let context = Unmanaged.passUnretained(self).toOpaque()
            cam_video_run(session, url.absoluteString, { context, frame in
                guard let context, let frame else { return }
                Unmanaged<VideoPlayer>.fromOpaque(context).takeUnretainedValue().receive(frame)
            }, context, &message, message.count)
            let reason = String(cString: message)
            DispatchQueue.main.async { self.onEnd?(reason) }
        }
    }

    @MainActor
    func stop() {
        onFrame = nil
        onEnd = nil
        cam_video_stop(session)
    }

    private func receive(_ frame: CVPixelBuffer) {
        mailbox.lock()
        pendingFrame = frame
        let shouldSchedule = !deliveryScheduled
        deliveryScheduled = true
        mailbox.unlock()

        if shouldSchedule {
            DispatchQueue.main.async { [self] in
                mailbox.lock()
                let frame = pendingFrame
                pendingFrame = nil
                deliveryScheduled = false
                mailbox.unlock()
                if let frame { onFrame?(frame) }
            }
        }
    }
}

@MainActor
final class VideoSurface: AVSampleBufferDisplayLayer {
    private var format: CMVideoFormatDescription?

    func display(_ image: CVPixelBuffer) {
        let renderer = sampleBufferRenderer
        if renderer.requiresFlushToResumeDecoding { renderer.flush() }
        guard renderer.isReadyForMoreMediaData else { return }
        if format.map({ CMVideoFormatDescriptionMatchesImageBuffer($0, imageBuffer: image) }) != true {
            CMVideoFormatDescriptionCreateForImageBuffer(allocator: nil, imageBuffer: image, formatDescriptionOut: &format)
        }
        guard let format else { return }
        var timing = CMSampleTimingInfo(duration: .invalid, presentationTimeStamp: .zero, decodeTimeStamp: .invalid)
        var sample: CMSampleBuffer?
        CMSampleBufferCreateReadyWithImageBuffer(allocator: nil, imageBuffer: image, formatDescription: format,
                                                  sampleTiming: &timing, sampleBufferOut: &sample)
        guard let sample,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true) else { return }
        let first = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
        CFDictionarySetValue(first, Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                             Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        renderer.enqueue(sample)
    }
}
