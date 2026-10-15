; Inno Setup script. Built by CI from the portable bundle:
;   iscc /DAppVersion=0.1.1 /DSourceDir=..\..\ferrum-windows-x86_64 ferrum.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\ferrum-windows-x86_64"
#endif

[Setup]
AppId={{7E3C8F4A-2B1D-4C6E-9F0A-5D8B1E2C3A47}
AppName=Ferrum
AppVersion={#AppVersion}
AppPublisher=Gadzhik
AppPublisherURL=https://github.com/Gadzhik/ferrum-rdp
DefaultDirName={autopf}\Ferrum
DefaultGroupName=Ferrum
UninstallDisplayIcon={app}\bin\ferrum.exe
LicenseFile={#SourceDir}\LICENSE
OutputDir=..\..
OutputBaseFilename=ferrum-{#AppVersion}-windows-x86_64-setup
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequiredOverridesAllowed=dialog
ChangesAssociations=yes
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{group}\Ferrum"; Filename: "{app}\bin\ferrum.exe"
Name: "{group}\{cm:UninstallProgram,Ferrum}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Ferrum"; Filename: "{app}\bin\ferrum.exe"; Tasks: desktopicon

[Registry]
; Offer Ferrum in "Open with" for .rdp files without taking them over from mstsc.
Root: HKA; Subkey: "Software\Classes\Ferrum.rdp"; ValueType: string; ValueData: "Remote Desktop Connection (Ferrum)"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Ferrum.rdp\shell\open\command"; ValueType: string; ValueData: """{app}\bin\ferrum.exe"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.rdp\OpenWithProgids"; ValueType: string; ValueName: "Ferrum.rdp"; ValueData: ""; Flags: uninsdeletevalue

[Run]
Filename: "{app}\bin\ferrum.exe"; Description: "{cm:LaunchProgram,Ferrum}"; Flags: nowait postinstall skipifsilent
