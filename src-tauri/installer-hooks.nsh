!include "${__FILEDIR__}\installer-webview.nsh"
; A normal NSIS upgrade replaces app files without running an uninstaller.
; Passive same-version repair has no radio HWNDs to read: use the same overlay.
; The caller skips only the choice page; install/config/runtime guards still run.
!macro FusionUpgradeChoice comparison wixMode passive showChoice
  StrCpy ${showChoice} 1
  ${If} ${wixMode} <> 1
    ${If} ${comparison} = 1
      StrCpy ${showChoice} 0
    ${ElseIf} ${comparison} = 0
    ${AndIf} ${passive} = 1
      StrCpy ${showChoice} 0
    ${EndIf}
  ${EndIf}
!macroend
; The updater launches NSIS immediately before exiting. Wait only for this
; installed file to be released; retain all maintenance checks afterwards.
!macro FusionWaitForUpdateExit directory
  !define FusionWaitID ${__LINE__}
  Push $0
  Push $1
  Push $2
  Push $3
  Push $R0
  Push $R2
  StrCpy $R2 0
  IfFileExists "${directory}\${MAINBINARYNAME}.exe" 0 fusion_wait_done_${FusionWaitID}
  !insertmacro RestartManager_StartSession $R0
  StrCmp $R0 "" fusion_wait_done_${FusionWaitID}
  !insertmacro RestartManager_RegisterFile $R0 "${directory}\${MAINBINARYNAME}.exe"
  StrCmp $0 0 0 fusion_wait_end_${FusionWaitID}
  fusion_wait_poll_${FusionWaitID}:
  StrCpy $1 0
  StrCpy $2 0
  System::Call 'RSTRTMGR::RmGetList(p R0, *i .r1, *i .r2, p 0, *i .r3) i .r0'
  StrCmp $0 0 fusion_wait_end_${FusionWaitID}
  IntOp $R2 $R2 + 1
  IntCmp $R2 100 fusion_wait_end_${FusionWaitID}
  Sleep 100
  Goto fusion_wait_poll_${FusionWaitID}
  fusion_wait_end_${FusionWaitID}:
  !insertmacro RestartManager_EndSession $R0
  fusion_wait_done_${FusionWaitID}:
  Pop $R2
  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
  !undef FusionWaitID
!macroend
; No RmShutdown/taskkill: even a silent upgrade must never interrupt an active request.
!macro FusionMaintenanceCheck directory mode
  !define FusionCheckID ${__LINE__}
  Push $0
  Push $1
  Push $2
  Push $3
  Push $R0
  Push $R1
  IfFileExists "${directory}\${MAINBINARYNAME}.exe" 0 fusion_no_process_${FusionCheckID}
  !insertmacro RestartManager_StartSession $R0
  StrCmp $R0 "" fusion_blocked_${FusionCheckID}
  !insertmacro RestartManager_RegisterFile $R0 "${directory}\${MAINBINARYNAME}.exe"
  StrCmp $0 0 0 fusion_session_blocked_${FusionCheckID}
  System::Call 'RSTRTMGR::RmGetList(p R0, *i .r1, *i .r2, p 0, *i .r3) i .r0'
  ; Only an unambiguous empty process list is safe. Never call RmShutdown.
  StrCmp $0 0 0 fusion_session_blocked_${FusionCheckID}
  !insertmacro RestartManager_EndSession $R0
  fusion_no_process_${FusionCheckID}:
  InitPluginsDir
  !if "${mode}" == "install"
    ; Use the NEW read-only checker, including when the installed version predates this switch.
    File /oname=$PLUGINSDIR\fusion-maintenance.exe "${MAINBINARYSRCPATH}"
  !else
    ; Never execute the file being checked: Restart Manager can briefly see an exiting checker.
    ClearErrors
    CopyFiles /SILENT "${directory}\${MAINBINARYNAME}.exe" "$PLUGINSDIR\fusion-maintenance.exe"
    IfErrors fusion_blocked_${FusionCheckID}
  !endif
  nsExec::ExecToStack /TIMEOUT=5000 '"$PLUGINSDIR\fusion-maintenance.exe" --check-uninstall-safe'
  Pop $0
  Pop $R1
  StrCmp $0 "0" fusion_safe_${FusionCheckID} fusion_blocked_${FusionCheckID}
  fusion_session_blocked_${FusionCheckID}:
  !insertmacro RestartManager_EndSession $R0
  fusion_blocked_${FusionCheckID}:
  IfSilent +2
  MessageBox MB_OK|MB_ICONEXCLAMATION "暂不能安装、升级或卸载 Fusion。请先在运行中的 Fusion 关闭账号池、恢复原接入，再从托盘退出。若程序已退出，请使用恢复官方接入入口处理遗留配置后重试。安装程序不会强制结束请求，也不会自动修改 Codex 配置。"
  Pop $R1
  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
  SetErrorLevel 2
  Abort
  fusion_safe_${FusionCheckID}:
  SetErrorLevel 0
  Pop $R1
  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
  !undef FusionCheckID
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro FusionMaintenanceCheck "$INSTDIR" uninstall
  !insertmacro FusionDeleteAppData
!macroend

; Called only after the process/config guard, before deleting the installed EXE.
!macro FusionDeleteAppData
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    nsExec::ExecToStack /TIMEOUT=30000 '"$PLUGINSDIR\fusion-maintenance.exe" --delete-app-data'
    Pop $0
    Pop $1
    ${If} $0 != "0"
      IfSilent +2
      MessageBox MB_OK|MB_ICONEXCLAMATION "Fusion 账号数据清理未完成，卸载已停止。请检查用户数据目录及凭据权限后重试。仅删除本工具的数据；Codex 与共享 WebView2 不受影响。"
      SetErrorLevel 3
      Abort
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  CreateShortcut "$SMPROGRAMS\Fusion 恢复官方接入.lnk" "$INSTDIR\personal-codex-pool.exe" "--recover-official"
  CreateDirectory "$LOCALAPPDATA\PersonalCodexPool\updates\repair"
  CreateShortcut "$SMPROGRAMS\Fusion 更新修复.lnk" "$LOCALAPPDATA\PersonalCodexPool\updates\repair"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  Delete "$SMPROGRAMS\Fusion 恢复官方接入.lnk"
  Delete "$SMPROGRAMS\Fusion 更新修复.lnk"
!macroend
