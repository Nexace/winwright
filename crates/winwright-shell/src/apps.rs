//! Store (packaged) apps by the name the Start menu shows: "WhatsApp", "Photos", "Spotify".
//! They have no program file to start; Windows activates them by their AppUserModelID, as the
//! Start menu does. They run in their package's sandbox and take no command line.

use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::UI::Shell::{
    AO_NONE, ApplicationActivationManager, BHID_EnumItems, FOLDERID_AppsFolder,
    IApplicationActivationManager, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT,
    SHGetKnownFolderItem, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};
use windows::core::{HSTRING, PWSTR};
use winwright_contracts::{WinwrightError, WinwrightResult};

/// Runs `f` on a short-lived thread with its own COM apartment.
fn with_com<T: Send + 'static>(
    f: impl FnOnce() -> windows::core::Result<T> + Send + 'static,
) -> WinwrightResult<T> {
    std::thread::spawn(move || {
        // SAFETY: initialised for this thread only and balanced below.
        let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let result = f();
        if init.is_ok() {
            // SAFETY: balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
        result
    })
    .join()
    .map_err(|_| WinwrightError::invalid("the app list could not be read"))?
    .map_err(|e| {
        WinwrightError::invalid(format!("the app list could not be read: {}", e.message()))
    })
}

fn name(item: &IShellItem, kind: SIGDN) -> Option<String> {
    // SAFETY: the returned string is ours to free once.
    unsafe {
        let raw: PWSTR = item.GetDisplayName(kind).ok()?;
        let text = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0.cast()));
        text
    }
}

/// How long the app list is reused: one launch looks it up several times (the prompt, the
/// launch), and installing an app is rare.
const LIST_REUSE: std::time::Duration = std::time::Duration::from_secs(30);

type AppList = Vec<(String, String)>;
static LISTED: std::sync::Mutex<Option<(std::time::Instant, AppList)>> =
    std::sync::Mutex::new(None);

/// Every packaged app in the Start menu's "All apps": (display name, AppUserModelID).
pub(crate) fn packaged_apps() -> WinwrightResult<AppList> {
    let mut listed = LISTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, apps)) = listed.as_ref()
        && at.elapsed() < LIST_REUSE
    {
        return Ok(apps.clone());
    }
    let apps = list_packaged_apps()?;
    *listed = Some((std::time::Instant::now(), apps.clone()));
    Ok(apps)
}

fn list_packaged_apps() -> WinwrightResult<AppList> {
    with_com(|| {
        // SAFETY: COM calls on interfaces owned by this thread.
        unsafe {
            let folder: IShellItem =
                SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)?;
            let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems)?;
            let mut apps = Vec::new();
            loop {
                let mut batch: [Option<IShellItem>; 32] = std::array::from_fn(|_| None);
                let mut fetched = 0;
                let _ = items.Next(&mut batch, Some(&mut fetched));
                if fetched == 0 {
                    break;
                }
                for item in batch.iter().take(fetched as usize).flatten() {
                    if let (Some(shown), Some(id)) = (
                        name(item, SIGDN_NORMALDISPLAY),
                        name(item, SIGDN_PARENTRELATIVEPARSING),
                    ) && is_app_id(&id)
                    {
                        apps.push((shown, id));
                    }
                }
            }
            Ok(apps)
        }
    })
}

/// `Family_publisherid!App`: what a packaged app's parsing name looks like (desktop entries are
/// paths or known-folder GUIDs instead).
pub(crate) fn is_app_id(id: &str) -> bool {
    match id.split_once('!') {
        Some((family, app)) => {
            family.contains('_') && !app.is_empty() && !id.contains(['\\', '/', ':', '{'])
        }
        None => false,
    }
}

/// Starts (or brings forward) a packaged app as the Start menu does; returns its process id.
pub(crate) fn activate(app_id: &str) -> WinwrightResult<u32> {
    let id = HSTRING::from(app_id);
    with_com(move || {
        // SAFETY: COM calls on an interface owned by this thread.
        unsafe {
            let manager: IApplicationActivationManager =
                CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
            manager.ActivateApplication(&id, &HSTRING::new(), AO_NONE)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_app_ids_are_told_from_desktop_entries() {
        assert!(is_app_id("5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App"));
        assert!(is_app_id("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"));
        assert!(!is_app_id(r"C:\Program Files\App\app.exe"));
        assert!(!is_app_id(
            "{6D809377-6AF0-444B-8957-A3773F02200E}\\app.exe"
        ));
        assert!(!is_app_id("Microsoft.Windows.Explorer"));
    }
}
