//! BGRA texture -> NV12 (GPU video processor) -> hardware H.264 (Media Foundation MFT).
//! On NVIDIA GPUs the hardware MFT is NVENC; AMD and Intel expose theirs the same way.

use crate::gpu::Gpu;
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, Interface};

const NV12_POOL: usize = 4;

pub struct Encoded {
    pub data: Bytes,
    pub keyframe: bool,
}

pub struct Encoder {
    gpu: Gpu,
    out_size: (u32, u32),
    fps: u32,
    frame_no: u64,

    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    enumerator: Option<ID3D11VideoProcessorEnumerator>,
    processor: Option<ID3D11VideoProcessor>,
    in_size: (u32, u32),
    nv12: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,

    mft: IMFTransform,
    events: IMFMediaEventGenerator,
    codec_api: ICodecAPI,
    need_input: u32,
    header: Vec<u8>,
    pub name: String,
}

fn variant_u32(v: u32) -> VARIANT {
    VARIANT::from(v)
}

impl Encoder {
    pub fn new(gpu: &Gpu, out_size: (u32, u32), fps: u32, kbps: u32) -> Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup")?;
        }

        let video_device: ID3D11VideoDevice = gpu.device.cast()?;
        let video_context: ID3D11VideoContext1 = gpu.context.cast()?;

        let (mft, name) = Self::create_mft()?;
        let events: IMFMediaEventGenerator = mft.cast()?;
        let codec_api: ICodecAPI = mft.cast()?;

        unsafe {
            let attrs = mft.GetAttributes()?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);

            // Let the encoder read our D3D11 textures directly.
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&gpu.device, token)?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .context("attaching the D3D manager to the encoder")?;
            std::mem::forget(manager); // lives as long as the encoder does
        }

        let mut enc = Self {
            gpu: gpu.clone(),
            out_size,
            fps,
            frame_no: 0,
            video_device,
            video_context,
            enumerator: None,
            processor: None,
            in_size: (0, 0),
            nv12: Vec::new(),
            mft,
            events,
            codec_api,
            need_input: 0,
            header: Vec::new(),
            name,
        };
        enc.configure(kbps)?;
        Ok(enc)
    }

    fn create_mft() -> Result<(IMFTransform, String)> {
        unsafe {
            let out_info = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_H264,
            };
            let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                None,
                Some(&out_info),
                &mut activates,
                &mut count,
            )?;
            if count == 0 || activates.is_null() {
                bail!("no hardware H.264 encoder found");
            }
            let list = std::slice::from_raw_parts(activates, count as usize);
            let mut found = None;
            for a in list.iter().flatten() {
                let mut len = 0u32;
                let mut buf = [0u16; 128];
                let mut name = String::from("hardware H.264 encoder");
                if a.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, Some(&mut len)).is_ok() {
                    name = String::from_utf16_lossy(&buf[..len as usize]);
                }
                if let Ok(t) = a.ActivateObject::<IMFTransform>() {
                    found = Some((t, name));
                    break;
                }
            }
            for a in std::slice::from_raw_parts_mut(activates, count as usize) {
                *a = None;
            }
            CoTaskMemFree(Some(activates as *const _));
            found.context("could not activate a hardware H.264 encoder")
        }
    }

    fn configure(&mut self, kbps: u32) -> Result<()> {
        let (w, h) = self.out_size;
        unsafe {
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, kbps * 1000)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(self.fps, 1))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
            out.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
            out.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
            out.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            out.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            self.mft.SetOutputType(0, &out, 0).context("setting the H.264 output type")?;

            let mut set_input = false;
            for i in 0.. {
                let Ok(t) = self.mft.GetInputAvailableType(0, i) else { break };
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
                    t.SetUINT64(&MF_MT_FRAME_RATE, pack(self.fps, 1))?;
                    t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
                    t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                    self.mft.SetInputType(0, &t, 0).context("setting the NV12 input type")?;
                    set_input = true;
                    break;
                }
            }
            if !set_input {
                bail!("encoder does not accept NV12 input");
            }

            let gop = self.fps * 2;
            let set = |key: &GUID, v: u32| {
                if let Err(e) = self.codec_api.SetValue(key, &variant_u32(v)) {
                    tracing::debug!("encoder setting {key:?} rejected: {e}");
                }
            };
            let _ = self.codec_api.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
            set(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32);
            set(&CODECAPI_AVEncCommonMeanBitRate, kbps * 1000);
            set(&CODECAPI_AVEncMPVGOPSize, gop);

            // Cached SPS/PPS, prepended to keyframes that arrive without them.
            let mut hdr_len = 0u32;
            let mut hdr = vec![0u8; 256];
            if let Ok(t) = self.mft.GetOutputCurrentType(0) {
                if t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut hdr, Some(&mut hdr_len)).is_ok() {
                    self.header = hdr[..hdr_len as usize].to_vec();
                }
            }

            self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
            self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(())
    }

    /// (Re)builds the colour converter whenever the captured size changes.
    fn ensure_processor(&mut self, in_size: (u32, u32)) -> Result<()> {
        if self.processor.is_some() && self.in_size == in_size {
            return Ok(());
        }
        let (ow, oh) = self.out_size;
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: self.fps, Denominator: 1 },
            InputWidth: in_size.0,
            InputHeight: in_size.1,
            OutputFrameRate: DXGI_RATIONAL { Numerator: self.fps, Denominator: 1 },
            OutputWidth: ow,
            OutputHeight: oh,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        unsafe {
            let enumerator = self.video_device.CreateVideoProcessorEnumerator(&content)?;
            let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;
            for (name, fmt) in [("BGRA", DXGI_FORMAT_B8G8R8A8_UNORM), ("NV12", DXGI_FORMAT_NV12)] {
                tracing::info!("video processor {name} support flags: {:?}", enumerator.CheckVideoProcessorFormat(fmt));
            }

            self.video_context.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            self.video_context.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );

            if self.nv12.is_empty() {
                for _ in 0..NV12_POOL {
                    let desc = D3D11_TEXTURE2D_DESC {
                        Width: ow,
                        Height: oh,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_NV12,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                        ..Default::default()
                    };
                    let mut tex = None;
                    self.gpu.device.CreateTexture2D(&desc, None, Some(&mut tex))?;
                    let tex = tex.context("no NV12 texture")?;
                    let vdesc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                        ..Default::default()
                    };
                    let mut view = None;
                    self.video_device.CreateVideoProcessorOutputView(
                        &tex,
                        &enumerator,
                        &vdesc,
                        Some(&mut view),
                    )?;
                    self.nv12.push((tex, view.context("no output view")?));
                }
            }
            self.enumerator = Some(enumerator);
            self.processor = Some(processor);
        }
        self.in_size = in_size;
        Ok(())
    }

    pub fn request_keyframe(&self) {
        unsafe {
            let _ = self.codec_api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1));
        }
    }

    /// Encodes one captured BGRA texture. Returns the encoded access units that came out
    /// (normally exactly one, occasionally none while the encoder is busy).
    pub fn encode(&mut self, bgra: &ID3D11Texture2D) -> Result<Vec<Encoded>> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { bgra.GetDesc(&mut desc) };
        self.ensure_processor((desc.Width, desc.Height))?;
        let enumerator = self.enumerator.as_ref().unwrap();
        let processor = self.processor.as_ref().unwrap();

        let slot = (self.frame_no % NV12_POOL as u64) as usize;
        let (nv12_tex, nv12_view) = &self.nv12[slot];

        unsafe {
            let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                },
            };
            let mut in_view = None;
            self.video_device
                .CreateVideoProcessorInputView(bgra, enumerator, &in_desc, Some(&mut in_view))
                .with_context(|| {
                    format!(
                        "input view for {}x{} format {:?} bind {:#x} misc {:#x} usage {:?} samples {}",
                        desc.Width, desc.Height, desc.Format, desc.BindFlags, desc.MiscFlags,
                        desc.Usage, desc.SampleDesc.Count
                    )
                })?;
            let in_view = in_view.context("no input view")?;

            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(in_view)),
                ..Default::default()
            };
            let blt = self.video_context.VideoProcessorBlt(
                processor,
                nv12_view,
                0,
                std::slice::from_ref(&stream),
            );
            drop(ManuallyDrop::into_inner(stream.pInputSurface));
            blt.context("colour conversion")?;

            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12_tex, 0, false)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            let step = 10_000_000i64 / self.fps as i64;
            sample.SetSampleTime(self.frame_no as i64 * step)?;
            sample.SetSampleDuration(step)?;

            self.pump_events(&mut Vec::new(), false)?;
            if self.need_input == 0 {
                // Encoder is still busy with the previous frame: drop this one.
                return Ok(Vec::new());
            }
            self.need_input -= 1;
            self.mft.ProcessInput(0, &sample, 0).context("feeding the encoder")?;
        }
        self.frame_no += 1;

        let mut out = Vec::new();
        self.pump_events(&mut out, true)?;
        Ok(out)
    }

    /// Handles the async MFT's event queue. With `wait`, blocks (briefly) until output arrives.
    fn pump_events(&mut self, out: &mut Vec<Encoded>, wait: bool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            let event = unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) };
            match event {
                Ok(ev) => {
                    let kind = unsafe { ev.GetType()? };
                    if kind == METransformNeedInput.0 as u32 {
                        self.need_input += 1;
                    } else if kind == METransformHaveOutput.0 as u32 {
                        if let Some(e) = self.take_output()? {
                            out.push(e);
                            if wait {
                                return Ok(());
                            }
                        }
                    }
                }
                Err(_) => {
                    if !wait || Instant::now() > deadline {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
        }
    }

    fn take_output(&mut self) -> Result<Option<Encoded>> {
        unsafe {
            let info = self.mft.GetOutputStreamInfo(0)?;
            let provides = info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32
                != 0;
            let own_sample = if provides {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1 << 20))?)?;
                Some(s)
            };
            let mut bufs = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(own_sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut bufs, &mut status);
            let sample = ManuallyDrop::into_inner(std::mem::replace(&mut bufs[0].pSample, ManuallyDrop::new(None)));
            drop(ManuallyDrop::into_inner(std::mem::replace(&mut bufs[0].pEvents, ManuallyDrop::new(None))));
            if let Err(e) = r {
                if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                    return Ok(None);
                }
                return Err(e).context("reading encoder output");
            }
            let Some(sample) = sample else { return Ok(None) };

            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let mut data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;

            let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0
                || has_nal(&data, 5);
            if keyframe && !has_nal(&data, 7) && !self.header.is_empty() {
                let mut with = self.header.clone();
                with.extend_from_slice(&data);
                data = with;
            }
            Ok(Some(Encoded { data: Bytes::from(data), keyframe }))
        }
    }
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

/// True if the Annex-B stream contains a NAL unit of `nal_type`.
pub fn has_nal(data: &[u8], nal_type: u8) -> bool {
    data.windows(4)
        .enumerate()
        .any(|(i, w)| w[..3] == [0, 0, 1] && w[3] & 0x1f == nal_type
            || (i + 4 < data.len() && w == [0, 0, 0, 1] && data[i + 4] & 0x1f == nal_type))
}

#[cfg(test)]
mod tests {
    use super::has_nal;

    #[test]
    fn finds_nal_types() {
        let stream = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x65, 9];
        assert!(has_nal(&stream, 7));
        assert!(has_nal(&stream, 5));
        assert!(!has_nal(&stream, 1));
    }
}
