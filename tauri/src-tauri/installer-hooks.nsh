; NSIS 安装 / 卸载钩子（由 tauri.conf.json 的 bundle.windows.nsis.installerHooks 引用）

; 卸载后清掉“开机自启”的注册表项，免得系统启动时去找一个已经不存在的程序。
; 自启项由 tauri-plugin-autostart 写在 HKCU\...\Run，名字是产品名；两个可能的名字都删，不存在时 DeleteRegValue 无副作用。
!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Agent Monitor"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "agent-monitor"
!macroend
