; NSIS installer hooks for ATEM IP Patchbay (Windows).
;
; Why this file exists: Tauri's NSIS bundler places everything from
; `bundle.resources` under subfolders of $INSTDIR (e.g.
; $INSTDIR\sidecar\Processing.NDI.Lib.x64.dll). Windows' default
; DLL-search at process startup checks the .exe's directory PLUS
; System32 / PATH, but NOT arbitrary subfolders of the .exe's dir.
; So the bundled NDI DLL is invisible to the loader at .exe launch
; time — atem-ip-patchbay.exe fails to start with a system error
; "Processing.NDI.Lib.x64.dll was not found" BEFORE any Rust code
; runs. SetDllDirectoryW (added in alpha.9 lib.rs startup) is too
; late: the loader has already given up by the time main() executes.
;
; Fix: copy the DLL from $INSTDIR\sidecar\ up to $INSTDIR\ right
; after install. Windows' default DLL search then finds it sitting
; next to the .exe at first launch. The duplicate file (~5 MB) is a
; small price for first-launch reliability without modifying any
; Rust code.
;
; Tauri 2 NSIS bundler hook macros (subset we use):
;   NSIS_HOOK_PREINSTALL    — before file extraction
;   NSIS_HOOK_POSTINSTALL   — after file extraction (this one)
;   NSIS_HOOK_PREUNINSTALL  — before file removal
;   NSIS_HOOK_POSTUNINSTALL — after file removal

!macro NSIS_HOOK_POSTINSTALL
  ; Copy NDI DLL up to $INSTDIR for Windows' loader to find at
  ; process startup. Idempotent: safe to re-run during repair
  ; installs (the destination just gets overwritten).
  IfFileExists "$INSTDIR\sidecar\Processing.NDI.Lib.x64.dll" 0 atem_ndi_dll_skip
    CopyFiles /SILENT "$INSTDIR\sidecar\Processing.NDI.Lib.x64.dll" "$INSTDIR\Processing.NDI.Lib.x64.dll"
  atem_ndi_dll_skip:

  ; alpha.18: same trick for libomt.dll + libvmx.dll. The libomt-rs
  ; crate statically links against libomt.lib at build time, so
  ; libomt.dll is in the .exe's import table — loaded by the
  ; Windows loader BEFORE main() runs, same constraint as the NDI
  ; DLL. libvmx is loaded by libomt at runtime; copying it up
  ; alongside is belt-and-suspenders for cases where libomt does
  ; an early dlopen.
  IfFileExists "$INSTDIR\sidecar\libomt.dll" 0 atem_omt_dll_skip
    CopyFiles /SILENT "$INSTDIR\sidecar\libomt.dll" "$INSTDIR\libomt.dll"
  atem_omt_dll_skip:
  IfFileExists "$INSTDIR\sidecar\libvmx.dll" 0 atem_vmx_dll_skip
    CopyFiles /SILENT "$INSTDIR\sidecar\libvmx.dll" "$INSTDIR\libvmx.dll"
  atem_vmx_dll_skip:
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Clean up the sibling DLLs we copied at install time. The
  ; originals under $INSTDIR\sidecar\ get removed by Tauri's
  ; standard uninstall sequence; the copies are ours alone to
  ; manage.
  Delete "$INSTDIR\Processing.NDI.Lib.x64.dll"
  Delete "$INSTDIR\libomt.dll"
  Delete "$INSTDIR\libvmx.dll"
!macroend
