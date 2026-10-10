; Inno Setup script for Vellum. Installs for the current user (no
; administrator rights needed), registers Vellum as a program that opens
; PDFs, and can put the command-line tool on the PATH.
;
; Build the release first (cargo build --release -j 6), then compile this
; script with Inno Setup 6:  ISCC installer\vellum.iss
; The installer is written to installer\output.

#define AppName "Vellum"
#define AppVersion "0.1.0"
#define BuildFolder "E:\VellumBuild\release"
#define PdfiumFolder "..\vendor\pdfium"

[Setup]
AppId={{AF179668-D50B-46E6-8245-EADEE685D488}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Vellum
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=output
OutputBaseFilename=Vellum-{#AppVersion}-setup
SetupIconFile=..\assets\vellum.ico
UninstallDisplayIcon={app}\vellum-app.exe
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes
ChangesEnvironment=yes

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked
Name: "openwith"; Description: "Offer Vellum for opening PDF files (choose it as the default in Windows Settings if you like)"
Name: "addtopath"; Description: "Add the vellum command-line tool to the PATH"; Flags: unchecked

[Files]
Source: "{#BuildFolder}\vellum-app.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildFolder}\vellum.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PdfiumFolder}\bin\pdfium.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PdfiumFolder}\LICENSE"; DestDir: "{app}\licenses"; DestName: "pdfium.txt"
Source: "{#PdfiumFolder}\licenses\*"; DestDir: "{app}\licenses\pdfium"

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\vellum-app.exe"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\vellum-app.exe"; Tasks: desktopicon

[Registry]
; A document type for PDFs opened in Vellum.
Root: HKA; Subkey: "Software\Classes\Vellum.Document"; ValueType: string; ValueData: "PDF Document"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Vellum.Document\DefaultIcon"; ValueType: string; ValueData: """{app}\vellum-app.exe"",0"; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Vellum.Document\shell\open\command"; ValueType: string; ValueData: """{app}\vellum-app.exe"" ""%1"""; Tasks: openwith
; Listed under "Open with" for .pdf, without taking over the current default.
Root: HKA; Subkey: "Software\Classes\.pdf\OpenWithProgids"; ValueType: string; ValueName: "Vellum.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\vellum-app.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "Vellum"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\vellum-app.exe\SupportedTypes"; ValueType: string; ValueName: ".pdf"; ValueData: ""; Tasks: openwith
; So Windows Settings can offer Vellum as the default PDF app.
Root: HKA; Subkey: "Software\Vellum\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "Vellum"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Vellum\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Read, edit, sign and redact PDF files."; Tasks: openwith
Root: HKA; Subkey: "Software\Vellum\Capabilities\FileAssociations"; ValueType: string; ValueName: ".pdf"; ValueData: "Vellum.Document"; Tasks: openwith
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "Vellum"; ValueData: "Software\Vellum\Capabilities"; Flags: uninsdeletevalue; Tasks: openwith
; The command-line tool on the user's PATH.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Check: NeedsAddPath(ExpandConstant('{app}')); Tasks: addtopath

[Run]
Filename: "{app}\vellum-app.exe"; Description: "Open Vellum"; Flags: nowait postinstall skipifsilent

[Code]
{ True when the folder is not on the user's PATH yet. }
function NeedsAddPath(Folder: string): Boolean;
var
  Paths: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Folder) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;

{ Takes the folder back off the PATH when uninstalling. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Paths, Folder: string;
  Position: Integer;
begin
  if CurUninstallStep <> usPostUninstall then
    exit;
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths) then
    exit;
  Folder := ExpandConstant('{app}');
  Position := Pos(';' + Uppercase(Folder), Uppercase(Paths));
  if Position > 0 then
  begin
    Delete(Paths, Position, Length(Folder) + 1);
    RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths);
  end;
end;
