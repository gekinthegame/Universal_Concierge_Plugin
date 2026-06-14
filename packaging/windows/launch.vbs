' Universal Concierge — Windows launcher.
' Runs the bundled CLI's `gui` command with a HIDDEN window (no console pops up),
' so clicking the Start Menu / desktop icon just opens the explorer in your browser.
Set sh  = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
dir = fso.GetParentFolderName(WScript.ScriptFullName)
sh.CurrentDirectory = dir
' windowStyle 0 = hidden, bWaitOnReturn = False (run detached).
sh.Run """" & dir & "\concierge-plugin.exe"" gui", 0, False
