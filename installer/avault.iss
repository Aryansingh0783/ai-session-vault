; Windows installer for AI Session Vault (Inno Setup 6).
; Build:  iscc /DAppVersion=2.0.2 /DSourceDir=<folder with avault.exe and avault-window.exe> installer\avault.iss
; Installs for the current user only (no administrator rights, no UAC prompt).

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\target\release"
#endif

#define AppName "AI Session Vault"
#define AppExe "avault-window.exe"

[Setup]
AppId={{8E2F5A64-3B1D-4C7A-9F0E-6D2A7B1C4E93}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Aryan Singh
AppPublisherURL=https://github.com/Aryansingh0783/ai-session-vault
AppSupportURL=https://github.com/Aryansingh0783/ai-session-vault/issues
AppUpdatesURL=https://github.com/Aryansingh0783/ai-session-vault/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist-installer
OutputBaseFilename=AI-Session-Vault-Setup-{#AppVersion}
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppName} setup

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"
Name: "startup"; Description: "Start AI Session Vault when I sign in to Windows (it keeps syncing from the tray)"; GroupDescription: "Background syncing:"
Name: "addtopath"; Description: "Add the &avault command to PATH (for the terminal)"; GroupDescription: "Command line:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\avault.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\avault-window.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\GUIDE.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autoprograms}\{#AppName} guide"; Filename: "{app}\GUIDE.md"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Start at login, hidden in the tray.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "AISessionVault"; ValueData: """{app}\{#AppExe}"" --hidden"; Flags: uninsdeletevalue; Tasks: startup
; avault on the user's PATH.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Check: NeedsAddPath(ExpandConstant('{app}')); Tasks: addtopath

[Run]
Filename: "{app}\{#AppExe}"; Description: "Open {#AppName}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM avault-window.exe"; Flags: runhidden; RunOnceId: "StopWindow"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM avault.exe"; Flags: runhidden; RunOnceId: "StopEngine"

[Code]
function NeedsAddPath(Param: string): Boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;

// Make sure a running copy is closed before files are replaced (upgrade in place).
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM avault-window.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM avault.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;
