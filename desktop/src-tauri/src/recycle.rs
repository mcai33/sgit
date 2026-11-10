use std::path::Path;

pub fn move_to_trash(path: &Path) -> sgit::Result<()> {
    recycle(path).map_err(|error| format!("Could not move the folder to the trash: {error}"))
}

#[cfg(target_os = "macos")]
fn recycle(path: &Path) -> Result<(), trash::Error> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};

    let mut context = trash::TrashContext::new();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn recycle(path: &Path) -> Result<(), trash::Error> {
    trash::delete(path)
}

#[cfg(windows)]
fn recycle(path: &Path) -> windows::core::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, IBindCtx,
            },
            UI::Shell::{
                FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, FOFX_EARLYFAILURE,
                FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IShellItem,
                SHCreateItemFromParsingName,
            },
        },
        core::{Error, HRESULT, PCWSTR},
    };

    struct ComApartment;
    impl Drop for ComApartment {
        fn drop(&mut self) {
            // The guard is created only after COM was initialized on this thread.
            unsafe { CoUninitialize() };
        }
    }

    let path: Vec<u16> = dunce::simplified(path)
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect();
    // All COM interfaces and the nul-terminated path remain valid for this operation.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _apartment = ComApartment;
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOperationFlags(
            FOFX_RECYCLEONDELETE
                | FOFX_EARLYFAILURE
                | FOF_NOERRORUI
                | FOF_SILENT
                | FOF_WANTNUKEWARNING,
        )?;
        let item: IShellItem =
            SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None::<&IBindCtx>)?;
        operation.DeleteItem(&item, None)?;
        operation.PerformOperations()?;
        if operation.GetAnyOperationsAborted()?.as_bool() {
            return Err(Error::new(
                HRESULT::from_win32(1223),
                "Recycle Bin operation canceled.",
            ));
        }
    }
    Ok(())
}
