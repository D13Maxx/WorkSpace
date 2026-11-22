; Hooks into Tauri's NSIS installer (tauri.conf.json: bundle.windows.nsis.installerHooks).

; Once Workspace is installed, let the user choose language packs: Workspace
; opens only its chooser window (packs.rs), and setup waits until it closes.
; Not for silent or passive installs, nor for updates, which should not ask again.
!macro NSIS_HOOK_POSTINSTALL
  ${IfNot} ${Silent}
  ${AndIf} $PassiveMode <> 1
  ${AndIf} $UpdateMode <> 1
    DetailPrint "Choosing language packs..."
    ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --choose-languages'
  ${EndIf}
!macroend
