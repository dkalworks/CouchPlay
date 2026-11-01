//! "Everything except this process tree" audio capture (Windows 10 2004+ process loopback),
//! used to keep Discord's voice chat out of the stream so friends don't hear themselves.

use anyhow::{Context, Result};
use std::mem::ManuallyDrop;
use std::sync::mpsc;
use std::time::Duration;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{BLOB, IAgileObject, IAgileObject_Impl};
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{Interface, Ref, implement, w};

/// Root of the first running process tree whose executable is one of `names` (case-insensitive).
/// A Discord install has many helper processes; the root is the one whose parent isn't Discord.
pub fn find_root_pid(names: &[String]) -> Option<u32> {
    let procs = list_processes()?;
    let wanted = |exe: &str| names.iter().any(|n| n.eq_ignore_ascii_case(exe));
    procs
        .iter()
        .filter(|(_, _, exe)| wanted(exe))
        .find(|(_, parent, _)| !procs.iter().any(|(pid, _, exe)| pid == parent && wanted(exe)))
        .map(|(pid, _, _)| *pid)
}

fn list_processes() -> Option<Vec<(u32, u32, String)>> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut out = Vec::new();
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            out.push((entry.th32ProcessID, entry.th32ParentProcessID, String::from_utf16_lossy(&entry.szExeFile[..len])));
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        Some(out)
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Done(mpsc::Sender<()>);

impl IAgileObject_Impl for Done_Impl {}

impl IActivateAudioInterfaceCompletionHandler_Impl for Done_Impl {
    fn ActivateCompleted(&self, _op: Ref<'_, IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

/// An audio client that hears every process except `pid` and its children.
pub fn client_excluding(pid: u32) -> Result<IAudioClient> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // The blob points at our stack: PROPVARIANT's Drop would CoTaskMemFree it.
    let prop = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: &mut params as *mut _ as *mut u8,
                    },
                },
            }),
        },
    });

    let (tx, rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Done(tx).into();
    let op = unsafe {
        ActivateAudioInterfaceAsync(
            w!("VAD\\Process_Loopback"),
            &IAudioClient::IID,
            Some(&*prop),
            &handler,
        )
    }
    .context("starting process-loopback activation")?;
    rx.recv_timeout(Duration::from_secs(5)).context("process-loopback activation timed out")?;

    let mut result = windows::core::HRESULT(0);
    let mut unknown = None;
    unsafe { op.GetActivateResult(&mut result, &mut unknown)? };
    result.ok().context("process-loopback activation failed")?;
    unknown.context("no audio client returned")?.cast().context("not an IAudioClient")
}
