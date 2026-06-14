; Universal Concierge — Windows installer (Inno Setup).
; Per-user install (no admin), Start Menu + optional desktop icon. The shortcuts
; point at launch.vbs so clicking them opens the explorer with no console window.
; Build:  ISCC.exe concierge.iss   (with concierge-plugin.exe, icon.ico, launch.vbs
; staged next to this script).

#define AppName "Universal Concierge"
#define AppVer "0.1.0"

[Setup]
AppId={{8F3A1C72-1E4D-4B9A-9C5E-2A7B6D0F1E22}
AppName={#AppName}
AppVersion={#AppVer}
AppPublisher=gekinthegame
WizardStyle=modern
DefaultDirName={localappdata}\Programs\Concierge
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=.
OutputBaseFilename=Universal-Concierge-windows-setup
SetupIconFile=icon.ico
UninstallDisplayIcon={app}\icon.ico
Compression=lzma
SolidCompression=yes

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop icon"; GroupDescription: "Additional icons:"

[Files]
Source: "concierge-plugin.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "icon.ico";             DestDir: "{app}"; Flags: ignoreversion
Source: "launch.vbs";           DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}";        Filename: "{app}\launch.vbs"; IconFilename: "{app}\icon.ico"
Name: "{autodesktop}\{#AppName}";  Filename: "{app}\launch.vbs"; IconFilename: "{app}\icon.ico"; Tasks: desktopicon
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"

[Run]
; Connect to Claude Code as an MCP server (best-effort, hidden).
Filename: "{app}\concierge-plugin.exe"; Parameters: "setup"; Flags: runhidden
Filename: "{app}\launch.vbs"; Description: "Launch {#AppName} now"; Flags: shellexec nowait postinstall skipifsilent
