; Inno Setup script for ATEM IP Patchbay.
;
; Driven by build/build.py — passes:
;   /DAppVersion=0.1.0
;   /DAppExePath=path\to\ATEM IP Patchbay.exe
;   /DOutputDir=path\to\build\dist
;
; Build manually:
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" build\installer.iss ^
;       /DAppVersion=0.1.0 ^
;       /DAppExePath=build\dist\ATEM IP Patchbay.exe ^
;       /DOutputDir=build\dist
;
; Produces ATEM-IP-Patchbay-Setup-<version>-x64.exe in OutputDir.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef AppExePath
  #define AppExePath "..\build\dist\ATEM IP Patchbay.exe"
#endif
#ifndef OutputDir
  #define OutputDir "..\build\dist"
#endif

#define MyAppName "ATEM IP Patchbay"
#define MyAppPublisher "Stephen Walter"
#define MyAppURL "https://github.com/amateurmenace/atem-ip-patchbay"
#define MyAppExeName "ATEM IP Patchbay.exe"

[Setup]
AppId={{A7E9D3F1-1B92-4B0C-9D8E-3C7E2F5A4B16}
AppName={#MyAppName}
AppVersion={#AppVersion}
AppVerName={#MyAppName} {#AppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=ATEM-IP-Patchbay-Setup-{#AppVersion}-x64
SetupIconFile=
Compression=lzma
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\{#MyAppExeName}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
Source: "{#AppExePath}"; DestDir: "{app}"; Flags: ignoreversion
; Ship a copy of the example config users can copy + edit.
Source: "..\config\example.xml"; DestDir: "{app}\config"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
; Offer to launch the app at the end of install — opens the local UI
; in the default browser via run.py's webbrowser.open().
Filename: "{app}\{#MyAppExeName}"; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent
