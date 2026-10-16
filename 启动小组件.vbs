' 无控制台窗口启动 Token 小组件（双击即可；退出请用托盘图标右键 → 退出）
Dim sh, dir
Set sh = CreateObject("WScript.Shell")
dir = CreateObject("Scripting.FileSystemObject").GetParentFolderName(WScript.ScriptFullName)
sh.CurrentDirectory = dir
sh.Run """" & dir & "\node_modules\electron\dist\electron.exe"" .", 0, False
