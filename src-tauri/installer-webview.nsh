; Fusion's single offline dependency path, shared by the production installer and isolated tests.
!ifndef FUSION_WEBVIEW_INCLUDED
!define FUSION_WEBVIEW_INCLUDED

!ifndef FUSION_WEBVIEW_CHECKER
!define FUSION_WEBVIEW_CHECKER "$PLUGINSDIR\fusion-maintenance.exe"
!endif
!macro FusionCheckWebViewRuntime
  ; The no-database helper asks the actual WebView2 loader, not a potentially stale registry key.
  nsExec::ExecToStack /TIMEOUT=10000 '"${FUSION_WEBVIEW_CHECKER}" --check-webview-runtime'
  Pop $R0
  Pop $R1
!macroend

!macro FusionEnsureWebView2
  !insertmacro FusionCheckWebViewRuntime
  ${If} $R0 != 0
    InitPluginsDir
    ; Unique NSIS private directory, never a predictable shared TEMP executable.
    File "/oname=$PLUGINSDIR\MicrosoftEdgeWebView2RuntimeInstaller.exe" "${WEBVIEW2INSTALLERPATH}"
    DetailPrint "正在离线补齐 WebView2 运行环境（可能需要几分钟）…"
    ClearErrors
    ExecWait '"$PLUGINSDIR\MicrosoftEdgeWebView2RuntimeInstaller.exe" /silent /install' $1
    ${If} ${Errors}
      StrCpy $1 "无法启动安装程序"
      Goto fusion_webview_failed
    ${EndIf}
    ${If} $1 = 3010
      SetRebootFlag true
    ; Microsoft winget manifest for this exact runtime lists these success codes.
    ; They never bypass the real Loader check below.
    ${ElseIf} $1 == -2147219416
    ${OrIf} $1 == -2147219187
      DetailPrint "WebView2 返回可接受状态，正在重新检查可用性…"
    ${ElseIf} $1 != 0
      Goto fusion_webview_failed
    ${EndIf}
    !insertmacro FusionCheckWebViewRuntime
    ${If} $R0 != 0
      StrCpy $1 "安装后仍未检测到所需版本；若系统要求重启，请重启后重试"
      Goto fusion_webview_failed
    ${EndIf}
  ${Else}
    DetailPrint "已检测到可用的 WebView2，跳过依赖安装"
  ${EndIf}
  Goto fusion_webview_ready
  fusion_webview_failed:
    IfSilent +2
    MessageBox MB_OK|MB_ICONSTOP "WebView2 离线安装未完成：$1。Fusion 尚未完成安装，请处理依赖后重试。"
    SetErrorLevel 3
    Abort
  fusion_webview_ready:
!macroend
!endif
