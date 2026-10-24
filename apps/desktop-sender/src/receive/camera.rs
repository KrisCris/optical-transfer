//! Camera capture for Receive mode.
//!
//! The camera that matters is an iPhone used as a macOS Continuity Camera: it
//! has a far better lens than any built-in webcam, it faces away from the user,
//! and it runs at 60 fps. The capture side asks for exactly what the web
//! receiver asks a browser for — 1280×960 luma at 60 fps — so field numbers
//! from either receiver mean the same thing.
//!
//! Only the luma plane is kept. A QR decoder binarises anyway, and taking the
//! `Y` plane of the camera's native biplanar format costs one row copy per
//! line rather than a colour conversion per pixel.

use std::sync::Arc;

/// One camera frame: tightly packed 8-bit luma, row-major.
#[derive(Debug)]
pub struct LumaFrame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// What a frame callback receives.
pub type FrameSink = Box<dyn Fn(Arc<LumaFrame>) + Send + Sync + 'static>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    /// Stable across launches; what a remembered choice keys on.
    pub id: String,
    pub name: String,
    /// An iPhone or iPad over Continuity Camera.
    pub continuity: bool,
    /// Part of the Mac or its display, so it faces the person at the keyboard.
    pub built_in: bool,
}

/// What the camera agreed to. Requested is not delivered: the capture rate in
/// telemetry is the measured one.
#[derive(Clone, Debug)]
pub struct Negotiated {
    pub name: String,
    pub width: usize,
    pub height: usize,
    pub fps: f64,
}

/// The output the web receiver requests.
pub const TARGET_WIDTH: u32 = 1280;
pub const TARGET_HEIGHT: u32 = 960;
pub const TARGET_FPS: f64 = 60.0;

/// One capture format a device offers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FormatOption {
    pub width: u32,
    pub height: u32,
    pub max_fps: f64,
}

/// The format to activate and what to ask the output to scale it to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FormatChoice {
    pub index: usize,
    /// Output size, when it differs from the format's own.
    pub scale_to: Option<(u32, u32)>,
    pub fps: f64,
}

/// Pick a capture format, in order of preference:
///
/// 1. exactly 1280×960 at 60 fps;
/// 2. the smallest 4:3 format of at least 1280×960 at 60 fps, scaled down to
///    1280×960 by the output — an iPhone offers 1920×1440 but not 1280×960;
/// 3. the largest format up to 1920 wide that still runs at 60 fps;
/// 4. whatever is largest up to 1920 wide, at its own best rate.
///
/// Frame rate outranks resolution: at 60 fps the fountain sees twice the
/// codes, and a v40 module is still over two pixels at 1280 wide.
pub fn choose_format(formats: &[FormatOption]) -> Option<FormatChoice> {
    const FAST: f64 = TARGET_FPS - 0.5;
    let fast = |format: &FormatOption| format.max_fps >= FAST;
    let four_three = |format: &FormatOption| format.width * 3 == format.height * 4;
    let area = |format: &FormatOption| u64::from(format.width) * u64::from(format.height);
    // Larger first, and landscape over the portrait twin of the same size:
    // the other screen is wider than it is tall.
    let size = |format: &FormatOption| (area(format), format.width);
    let indexed = || formats.iter().enumerate();

    if let Some((index, _)) = indexed().find(|(_, format)| {
        fast(format) && format.width == TARGET_WIDTH && format.height == TARGET_HEIGHT
    }) {
        return Some(FormatChoice {
            index,
            scale_to: None,
            fps: TARGET_FPS,
        });
    }
    if let Some((index, _)) = indexed()
        .filter(|(_, format)| fast(format) && four_three(format) && format.width >= TARGET_WIDTH)
        .min_by_key(|(_, format)| area(format))
    {
        return Some(FormatChoice {
            index,
            scale_to: Some((TARGET_WIDTH, TARGET_HEIGHT)),
            fps: TARGET_FPS,
        });
    }
    if let Some((index, _)) = indexed()
        .filter(|(_, format)| fast(format) && format.width <= 1920)
        .max_by_key(|(_, format)| size(format))
    {
        return Some(FormatChoice {
            index,
            scale_to: None,
            fps: TARGET_FPS,
        });
    }
    indexed()
        .filter(|(_, format)| format.width <= 1920)
        .max_by(|(_, a), (_, b)| size(a).cmp(&size(b)).then(a.max_fps.total_cmp(&b.max_fps)))
        .or_else(|| indexed().min_by_key(|(_, format)| area(format)))
        .map(|(index, format)| FormatChoice {
            index,
            scale_to: None,
            fps: format.max_fps,
        })
}

/// The camera to open when the user has not picked one: a Continuity Camera
/// if there is one — on a Mac the built-in camera faces the user, not the
/// other screen — then any other real camera, then whatever there is. The
/// same order as the web receiver's `pickDesktopCamera`.
pub fn preferred(cameras: &[CameraInfo]) -> Option<&CameraInfo> {
    cameras
        .iter()
        .find(|camera| camera.continuity)
        .or_else(|| {
            cameras
                .iter()
                .find(|camera| !camera.built_in && !looks_virtual(&camera.name))
        })
        .or_else(|| cameras.first())
}

/// Software cameras that re-broadcast something else rather than film.
fn looks_virtual(name: &str) -> bool {
    const VIRTUAL: [&str; 9] = [
        "virtual",
        "obs",
        "snap camera",
        "ndi",
        "mmhmm",
        "loom",
        "zoom",
        "teams",
        "screen",
    ];
    let name = name.to_lowercase();
    VIRTUAL.iter().any(|word| name.contains(word))
}

/// Names a phone camera reports when the OS cannot say so directly.
pub fn looks_like_continuity(name: &str) -> bool {
    name.split(|character: char| !character.is_alphanumeric())
        .any(|word| word.eq_ignore_ascii_case("iphone") || word.eq_ignore_ascii_case("ipad"))
}

/// A device name as the UI can draw it. Phone names are user-chosen, and
/// "𝕊𝕒𝕞's iPhone" in mathematical letters renders as a row of empty boxes in
/// the UI font; NFKC folds those letters to plain ones and leaves everything
/// else, accents and CJK included, as it was.
#[cfg(feature = "receive")]
pub fn display_name(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    name.nfkc().collect()
}

#[cfg(all(feature = "receive", target_os = "macos"))]
pub use macos::{Camera, cameras, open};

#[cfg(not(all(feature = "receive", target_os = "macos")))]
pub use unsupported::{Camera, cameras, open};

#[cfg(not(all(feature = "receive", target_os = "macos")))]
mod unsupported {
    use super::{CameraInfo, FrameSink, Negotiated};

    pub struct Camera;

    pub fn cameras() -> Vec<CameraInfo> {
        Vec::new()
    }

    pub fn open(_id: Option<&str>, _sink: FrameSink) -> Result<(Camera, Negotiated), String> {
        Err("Camera capture is only available in the macOS build so far.".to_owned())
    }
}

#[cfg(all(feature = "receive", target_os = "macos"))]
#[allow(unsafe_code)]
mod macos {
    //! AVFoundation through `objc2`.
    //!
    //! Device types are passed by name rather than through the framework's
    //! exported constants: the app runs on macOS 11, and the Continuity and
    //! External types only exist from macOS 14. Linking their symbols would
    //! stop the app launching at all on anything older.

    use std::{
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };

    use block2::RcBlock;
    use dispatch2::DispatchQueue;
    use objc2::{
        AnyThread, DefinedClass, define_class, msg_send,
        rc::Retained,
        runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject},
        sel,
    };
    use objc2_av_foundation::{
        AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice,
        AVCaptureDeviceDiscoverySession, AVCaptureDeviceInput, AVCaptureDevicePosition,
        AVCaptureOutput, AVCaptureSession, AVCaptureVideoDataOutput,
        AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
    };
    use objc2_core_media::{CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetDimensions};
    use objc2_core_video::{
        CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
        CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    };
    use objc2_foundation::{
        NSArray, NSBundle, NSDictionary, NSNumber, NSOperatingSystemVersion, NSProcessInfo,
        NSString,
    };

    use super::{
        CameraInfo, FormatOption, FrameSink, LumaFrame, Negotiated, choose_format,
        looks_like_continuity,
    };

    /// A running capture session. Dropping it stops the camera.
    ///
    /// The session lives on a thread of its own: `startRunning` blocks for as
    /// long as the device takes to wake, which must not be the UI's frame.
    pub struct Camera {
        stop: Option<mpsc::Sender<()>>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Drop for Camera {
        fn drop(&mut self) {
            drop(self.stop.take());
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn at_least(major: isize, minor: isize) -> bool {
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: major,
            minorVersion: minor,
            patchVersion: 0,
        })
    }

    /// Without this Info.plist key macOS reports a Continuity Camera as an
    /// ordinary built-in or external camera, and asking for the Continuity
    /// type only earns a console warning. The packaged app sets it; a
    /// `cargo run` binary has no Info.plist at all.
    fn continuity_type_enabled() -> bool {
        let key = NSString::from_str("NSCameraUseContinuityCameraDeviceType");
        NSBundle::mainBundle()
            .objectForInfoDictionaryKey(&key)
            .and_then(|value| value.downcast::<NSNumber>().ok())
            .is_some_and(|value| value.boolValue())
    }

    fn discovery() -> Retained<AVCaptureDeviceDiscoverySession> {
        let mut names = Vec::new();
        if at_least(14, 0) {
            if continuity_type_enabled() {
                names.push("AVCaptureDeviceTypeContinuityCamera");
            }
            names.push("AVCaptureDeviceTypeExternal");
        } else {
            names.push("AVCaptureDeviceTypeExternalUnknown");
        }
        names.push("AVCaptureDeviceTypeBuiltInWideAngleCamera");
        let types: Vec<Retained<NSString>> = names.into_iter().map(NSString::from_str).collect();
        let types = NSArray::from_retained_slice(&types);
        unsafe {
            AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
                &types,
                AVMediaTypeVideo,
                AVCaptureDevicePosition::Unspecified,
            )
        }
    }

    fn info(device: &AVCaptureDevice) -> CameraInfo {
        let name = super::display_name(&unsafe { device.localizedName() }.to_string());
        // `isContinuityCamera` arrived in macOS 13; before that the name is
        // all there is to go on.
        let flagged = at_least(13, 0)
            && device.respondsToSelector(sel!(isContinuityCamera))
            && unsafe { device.isContinuityCamera() };
        let built_in = unsafe { device.deviceType() }.to_string()
            == "AVCaptureDeviceTypeBuiltInWideAngleCamera";
        CameraInfo {
            id: unsafe { device.uniqueID() }.to_string(),
            continuity: flagged || looks_like_continuity(&name),
            built_in,
            name,
        }
    }

    pub fn cameras() -> Vec<CameraInfo> {
        let mut cameras: Vec<CameraInfo> = unsafe { discovery().devices() }
            .iter()
            .map(|device| info(&device))
            .collect();
        cameras.sort_by_key(|camera| !camera.continuity);
        cameras
    }

    /// Ask for camera access, blocking until the user answers the prompt.
    fn authorize() -> Result<(), String> {
        let media = unsafe { AVMediaTypeVideo }.ok_or("This Mac reports no video capture.")?;
        let refused = || {
            "Camera access was refused. Allow it in System Settings → Privacy & Security → \
             Camera, then try again."
                .to_owned()
        };
        match unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) } {
            AVAuthorizationStatus::Authorized => Ok(()),
            AVAuthorizationStatus::NotDetermined => {
                let (sender, answer) = mpsc::channel();
                let block = RcBlock::new(move |granted: Bool| {
                    let _ = sender.send(granted.as_bool());
                });
                unsafe {
                    AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block)
                };
                if answer.recv().unwrap_or(false) {
                    Ok(())
                } else {
                    Err(refused())
                }
            }
            AVAuthorizationStatus::Restricted => {
                Err("Camera access is restricted on this Mac by a device policy.".to_owned())
            }
            _ => Err(refused()),
        }
    }

    pub struct SinkIvars {
        sink: FrameSink,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and this class
        // implements no `Drop`.
        #[unsafe(super(NSObject))]
        #[name = "OpticalTransferFrameSink"]
        #[ivars = SinkIvars]
        struct FrameDelegate;

        unsafe impl NSObjectProtocol for FrameDelegate {}

        unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for FrameDelegate {
            #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
            fn capture_output(
                &self,
                _output: &AVCaptureOutput,
                sample: &CMSampleBuffer,
                _connection: &AVCaptureConnection,
            ) {
                if let Some(frame) = luma_of(sample) {
                    (self.ivars().sink)(Arc::new(frame));
                }
            }
        }
    );

    impl FrameDelegate {
        fn new(sink: FrameSink) -> Retained<Self> {
            let this = Self::alloc().set_ivars(SinkIvars { sink });
            unsafe { msg_send![super(this), init] }
        }
    }

    /// Copy the `Y` plane out of a biplanar frame, dropping row padding.
    fn luma_of(sample: &CMSampleBuffer) -> Option<LumaFrame> {
        let buffer = unsafe { sample.image_buffer() }?;
        // SAFETY: a CVPixelBuffer from a live sample buffer; unlocked below
        // with the same flags on every path.
        if unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) } != 0 {
            return None;
        }
        let width = CVPixelBufferGetWidthOfPlane(&buffer, 0);
        let height = CVPixelBufferGetHeightOfPlane(&buffer, 0);
        let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, 0);
        let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, 0).cast::<u8>();
        let frame = (!base.is_null() && width > 0 && height > 0 && stride >= width).then(|| {
            // SAFETY: the buffer is locked, and plane 0 is `height` rows of
            // `stride` bytes starting at `base`.
            let plane = unsafe { std::slice::from_raw_parts(base, stride * height) };
            let mut pixels = Vec::with_capacity(width * height);
            for row in plane.chunks_exact(stride) {
                pixels.extend_from_slice(&row[..width]);
            }
            LumaFrame {
                width,
                height,
                pixels,
            }
        });
        unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) };
        frame
    }

    /// Open a camera — the one with this id, else the preferred one — and
    /// start delivering frames to `sink` on a capture queue.
    pub fn open(id: Option<&str>, sink: FrameSink) -> Result<(Camera, Negotiated), String> {
        authorize()?;
        let id = id.map(str::to_owned);
        let (ready, outcome) = mpsc::channel();
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = thread::Builder::new()
            .name("camera".to_owned())
            .spawn(move || {
                let session = match start(id.as_deref(), sink) {
                    Ok((session, negotiated)) => {
                        let _ = ready.send(Ok(negotiated));
                        session
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                // Park until the handle is dropped; the frames arrive on the
                // capture queue, not here.
                let _ = stopped.recv();
                unsafe { session.stopRunning() };
            })
            .map_err(|error| format!("The camera thread could not start: {error}."))?;
        let negotiated = outcome
            .recv_timeout(Duration::from_secs(15))
            .map_err(|_| "The camera did not start.".to_owned())??;
        Ok((
            Camera {
                stop: Some(stop),
                thread: Some(thread),
            },
            negotiated,
        ))
    }

    fn start(
        id: Option<&str>,
        sink: FrameSink,
    ) -> Result<(Retained<AVCaptureSession>, Negotiated), String> {
        let devices = unsafe { discovery().devices() };
        let infos: Vec<CameraInfo> = devices.iter().map(|device| info(&device)).collect();
        let index = id
            .and_then(|id| infos.iter().position(|camera| camera.id == id))
            .or_else(|| infos.iter().position(|camera| camera.continuity))
            .or((!infos.is_empty()).then_some(0))
            .ok_or("No camera found. Connect one, or bring an iPhone near this Mac.")?;
        let device = devices.objectAtIndex(index);
        let name = infos[index].name.clone();

        let formats = unsafe { device.formats() };
        let options: Vec<FormatOption> = formats
            .iter()
            .map(|format| {
                let dimensions =
                    unsafe { CMVideoFormatDescriptionGetDimensions(&format.formatDescription()) };
                let max_fps = unsafe { format.videoSupportedFrameRateRanges() }
                    .iter()
                    .map(|range| unsafe { range.maxFrameRate() })
                    .fold(0.0, f64::max);
                FormatOption {
                    width: dimensions.width.max(0) as u32,
                    height: dimensions.height.max(0) as u32,
                    max_fps,
                }
            })
            .collect();
        let choice = choose_format(&options).ok_or("That camera offers no video formats.")?;
        let format = formats.objectAtIndex(choice.index);
        let option = options[choice.index];

        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }.map_err(
            |error| {
                format!(
                    "{name} could not be opened: {}.",
                    error.localizedDescription()
                )
            },
        )?;
        let session = unsafe { AVCaptureSession::new() };
        let output = unsafe { AVCaptureVideoDataOutput::new() };
        let (width, height) = choice.scale_to.unwrap_or((option.width, option.height));
        unsafe {
            // The CoreVideo dictionary keys are CFStrings whose values are
            // these names; NSString keys with the same text are equal to them.
            let keys = [
                NSString::from_str("PixelFormatType"),
                NSString::from_str("Width"),
                NSString::from_str("Height"),
            ];
            let number = |value: u32| -> Retained<AnyObject> {
                Retained::into_super(Retained::into_super(NSNumber::new_u32(value))).into()
            };
            let values = [
                number(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange),
                number(width),
                number(height),
            ];
            let key_refs: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
            let settings = NSDictionary::from_retained_objects(&key_refs, &values);
            output.setVideoSettings(Some(&settings));
            // A late frame is a stale frame: the fountain prefers the next one.
            output.setAlwaysDiscardsLateVideoFrames(true);

            session.beginConfiguration();
            if !session.canAddInput(&input) || !session.canAddOutput(&output) {
                session.commitConfiguration();
                return Err(format!("{name} cannot be used for video capture."));
            }
            session.addInput(&input);
            session.addOutput(&output);
            // The format goes on after the input is added, or the session's
            // preset overrides it.
            if device.lockForConfiguration().is_ok() {
                device.setActiveFormat(&format);
                let frame = CMTime::new(1000, (choice.fps * 1000.0).round() as i32);
                device.setActiveVideoMinFrameDuration(frame);
                device.setActiveVideoMaxFrameDuration(frame);
                device.unlockForConfiguration();
            }
            session.commitConfiguration();
        }

        let delegate = FrameDelegate::new(sink);
        let queue = DispatchQueue::new("optical-transfer.camera", None);
        unsafe {
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );
            session.startRunning();
        }
        if !unsafe { session.isRunning() } {
            return Err(format!(
                "{name} would not start. Another app may be using it."
            ));
        }
        Ok((
            session,
            Negotiated {
                name,
                width: width as usize,
                height: height as usize,
                fps: choice.fps,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(width: u32, height: u32, max_fps: f64) -> FormatOption {
        FormatOption {
            width,
            height,
            max_fps,
        }
    }

    /// The formats two iPhones offered over Continuity Camera on macOS 15.
    /// Neither offers 1280×960.
    fn iphone() -> Vec<FormatOption> {
        vec![
            format(640, 480, 30.0),
            format(640, 480, 60.0),
            format(1280, 720, 30.0),
            format(1280, 720, 60.0),
            format(1920, 1080, 30.0),
            format(1920, 1080, 60.0),
            format(1920, 1440, 30.0),
            format(1920, 1440, 60.0),
        ]
    }

    #[test]
    fn an_exact_match_wins() {
        let mut formats = iphone();
        formats.push(format(1280, 960, 60.0));
        assert_eq!(
            choose_format(&formats),
            Some(FormatChoice {
                index: 8,
                scale_to: None,
                fps: 60.0
            })
        );
    }

    #[test]
    fn an_iphone_scales_its_4_3_format_down_to_the_web_size() {
        assert_eq!(
            choose_format(&iphone()),
            Some(FormatChoice {
                index: 7,
                scale_to: Some((1280, 960)),
                fps: 60.0
            })
        );
    }

    #[test]
    fn without_4_3_the_largest_60_fps_format_is_kept_as_is() {
        let formats = [format(1280, 720, 60.0), format(3840, 2160, 60.0)];
        assert_eq!(
            choose_format(&formats),
            Some(FormatChoice {
                index: 0,
                scale_to: None,
                fps: 60.0
            })
        );
    }

    #[test]
    fn a_30_fps_webcam_falls_back_to_its_largest_sensible_format() {
        // The formats a FaceTime HD camera offered.
        let formats = [
            format(1920, 1080, 30.0),
            format(1280, 720, 30.0),
            format(1080, 1920, 30.0),
            format(640, 480, 30.0),
        ];
        assert_eq!(
            choose_format(&formats),
            Some(FormatChoice {
                index: 0,
                scale_to: None,
                fps: 30.0
            })
        );
        assert_eq!(choose_format(&[]), None);
        assert_eq!(
            choose_format(&[format(4096, 3072, 24.0)]).map(|choice| choice.index),
            Some(0)
        );
    }

    #[cfg(feature = "receive")]
    #[test]
    fn styled_device_names_fold_to_drawable_text() {
        assert_eq!(display_name("𝕒𝕡𝕖𝕣𝕥𝕦𝕣𝕖 Camera"), "aperture Camera");
        assert_eq!(display_name("𝕊ℍ𝟜𝔻𝟘𝕎 Camera"), "SH4D0W Camera");
        assert_eq!(display_name("Zoë’s iPhone 相机"), "Zoë’s iPhone 相机");
    }

    #[test]
    fn a_continuity_camera_is_preferred_over_the_built_in_one() {
        let cameras = [
            CameraInfo {
                id: "built-in".into(),
                name: "FaceTime HD Camera".into(),
                continuity: false,
                built_in: true,
            },
            CameraInfo {
                id: "phone".into(),
                name: "Sam's iPhone Camera".into(),
                continuity: true,
                built_in: false,
            },
        ];
        assert_eq!(
            preferred(&cameras).map(|camera| camera.id.as_str()),
            Some("phone")
        );
        assert_eq!(
            preferred(&cameras[..1]).map(|camera| camera.id.as_str()),
            Some("built-in")
        );
        assert!(looks_like_continuity("Sam’s iPhone Camera"));
        assert!(looks_like_continuity("iPad (2) Camera"));
        assert!(!looks_like_continuity("Logitech BRIO"));
        assert!(!looks_like_continuity("iPhoneography Cam"));
    }

    #[test]
    fn without_a_flagged_phone_an_external_camera_beats_the_built_in_one() {
        let camera = |id: &str, name: &str, built_in| CameraInfo {
            id: id.into(),
            name: name.into(),
            continuity: false,
            built_in,
        };
        let cameras = [
            camera("built-in", "FaceTime HD Camera", true),
            camera("obs", "OBS Virtual Camera", false),
            camera("phone", "SH4D0W Camera", false),
        ];
        assert_eq!(
            preferred(&cameras).map(|camera| camera.id.as_str()),
            Some("phone")
        );
        assert_eq!(
            preferred(&cameras[..2]).map(|camera| camera.id.as_str()),
            Some("built-in"),
            "a virtual camera is never preferred over a real one"
        );
    }
}
