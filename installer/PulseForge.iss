; PulseForge installer (Inno Setup 6). Installs for the current user only: no administrator rights needed.

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif

[Setup]
AppId={{6F0C2B8E-91D4-4A7B-B3E5-2C9A7D4F8E13}
AppName=PulseForge
AppVersion={#AppVersion}
AppVerName=PulseForge {#AppVersion}
AppPublisher=Zheer Khalid
AppPublisherURL=https://zheer.dev
AppSupportURL=https://github.com/zheerkhalid/pulseforge
AppUpdatesURL=https://github.com/zheerkhalid/pulseforge/releases
DefaultDirName={autopf}\PulseForge
DefaultGroupName=PulseForge
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible 
ArchitecturesInstallIn64BitMode=x64compatible 
OutputDir=..\dist
OutputBaseFilename=PulseForge-Setup
SetupIconFile=..\target\pulseforge.ico
UninstallDisplayIcon={app}\PulseForge.exe
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
LicenseFile=..\LICENSE
VersionInfoVersion={#AppVersion}
VersionInfoCompany=Zheer Khalid (zheer.dev)
VersionInfoDescription=PulseForge Setup

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; 

[Files]
Source: "..\target\release\PulseForge.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\PulseForge"; Filename: "{app}\PulseForge.exe"
Name: "{group}\Uninstall PulseForge"; Filename: "{uninstallexe}"
Name: "{autodesktop}\PulseForge"; Filename: "{app}\PulseForge.exe"; Tasks: desktopicon

[Registry]
; The optional "start with Windows" entry is created by the program itself
; (Settings). Setup never creates it, but removes it on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "PulseForge"; Flags: dontcreatekey uninsdeletevalue

[Run]
Filename: "{app}\PulseForge.exe"; Description: "{cm:LaunchProgram,PulseForge}"; Flags: nowait postinstall skipifsilent shellexec

[UninstallRun]
Filename: "{cmd}"; Parameters: "/C taskkill /IM PulseForge.exe /F"; Flags: runhidden; RunOnceId: "KillPulseForge"
