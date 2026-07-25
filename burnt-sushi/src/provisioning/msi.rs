use std::path::Path;

use anyhow::Context;
use widestring::{U16CString, u16cstr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_SUCCESS},
    System::{
        ApplicationInstallationAndServicing::MsiEnumRelatedProductsW,
        Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
    },
    UI::{
        Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_HIDE,
    },
};

/// `UpgradeCode` from `wix/main.wxs`, constant across all versions of the MSI.
const UPGRADE_CODE: &str = env!("WIX_UPGRADE_CODE");

/// `UpgradeCode` used by per-machine installs (<= 0.3.3), before the switch to per-user.
const LEGACY_UPGRADE_CODE: &str = "31d49aef-51d9-4e4e-ac9e-bb0ebfdebca1";

/// Returns whether the app is installed via an MSI, as opposed to running as a portable exe.
pub fn is_installed() -> bool {
    find_related_product_code(UPGRADE_CODE).is_some()
}

/// Registered under the legacy `UpgradeCode` but not the current one.
pub fn is_running_from_legacy_install() -> bool {
    !is_installed() && find_related_product_code(LEGACY_UPGRADE_CODE).is_some()
}

fn find_related_product_code(upgrade_code: &str) -> Option<U16CString> {
    let upgrade_code = U16CString::from_str(format!("{{{upgrade_code}}}")).unwrap();
    let mut product_buf = [0u16; 39];
    let result =
        unsafe { MsiEnumRelatedProductsW(upgrade_code.as_ptr(), 0, 0, product_buf.as_mut_ptr()) };
    (result == ERROR_SUCCESS).then(|| U16CString::from_vec_truncate(product_buf))
}

pub async fn upgrade(msi_path: &Path, install_dir: &Path) -> anyhow::Result<()> {
    let parameters = format!(
        "/i \"{}\" APPLICATIONFOLDER=\"{}\" /quiet /norestart",
        msi_path.display(),
        install_dir.display()
    );

    tokio::task::spawn_blocking(move || run_msiexec("open", &parameters))
        .await
        .context("Failed to run msiexec")?
}

/// Removes a leftover per-machine install from before the switch to per-user installs.
pub async fn cleanup_legacy_install() -> anyhow::Result<()> {
    let Some(product_code) = find_related_product_code(LEGACY_UPGRADE_CODE) else {
        return Ok(());
    };

    let parameters = format!(
        "/x \"{}\" /quiet /norestart",
        product_code.to_string_lossy()
    );

    tokio::task::spawn_blocking(move || run_msiexec("runas", &parameters))
        .await
        .context("Failed to run msiexec")?
}

/// Runs `msiexec` via `ShellExecuteExW`.
fn run_msiexec(verb: &str, parameters: &str) -> anyhow::Result<()> {
    let file = u16cstr!("msiexec.exe");
    let verb = U16CString::from_str(verb).unwrap();
    let parameters =
        U16CString::from_str(parameters).context("MSI arguments contain invalid characters")?;

    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: SW_HIDE,
        ..unsafe { std::mem::zeroed() }
    };

    let launched = unsafe { ShellExecuteExW(&raw mut info) };
    if launched == 0 || info.hProcess.is_null() {
        return Err(std::io::Error::last_os_error()).context("Failed to launch msiexec");
    }

    let exit_code = unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);

        let mut exit_code = 0u32;
        let got_exit_code = GetExitCodeProcess(info.hProcess, &raw mut exit_code) != 0;
        CloseHandle(info.hProcess);

        if !got_exit_code {
            anyhow::bail!("Failed to get msiexec exit code");
        }
        exit_code
    };

    if exit_code != 0 {
        anyhow::bail!("msiexec exited with code {exit_code}");
    }

    Ok(())
}
