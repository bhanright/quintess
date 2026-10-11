; Inno Setup script for Quintess. Installs for the current user (no
; administrator rights needed), registers Quintess as a program that opens
; PDFs, and puts the quintess command on the user's PATH.
;
; One program, quintess.exe, is both the app and the command-line tool. It
; is a Windows (GUI) program, so opening a PDF shows no console window.
; quintess.com, the small console launcher (crates/quintess-console), sits
; beside it so that typing `quintess` in cmd or PowerShell, which try .com
; before .exe, behaves like any console program.
;
; Before compiling (see docs/BUILDING.md):
;   1. cargo build --release -j 6
;   2. python tools/third_party_notices.py
;   3. put the OCR models (text-detection.onnx, text-recognition.onnx) in
;      vendor\ocrs-models; `quintess ocr` downloads them once to
;      %LOCALAPPDATA%\Quintess\models, from where they can be copied
; then compile with Inno Setup 6:  ISCC installer\quintess.iss
; The installer is written to installer\output. Set QUINTESS_BUILD to the
; Cargo target folder when it isn't ..\target.

#define AppName "Quintess"
#define AppVersion "0.2.1"
#define BuildFolder (GetEnv("QUINTESS_BUILD") != "" ? GetEnv("QUINTESS_BUILD") : "..\target") + "\release"
#define PdfiumFolder "..\vendor\pdfium"
#define ModelsFolder "..\vendor\ocrs-models"

[Setup]
AppId={{AF179668-D50B-46E6-8245-EADEE685D488}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=William Hanright
AppPublisherURL=https://quintess.cc
AppSupportURL=https://github.com/bhanright/quintess/issues
AppUpdatesURL=https://github.com/bhanright/quintess/releases
AppCopyright=Copyright (c) 2026 William Hanright
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=output
OutputBaseFilename=Quintess-{#AppVersion}-setup
SetupIconFile=..\assets\quintess.ico
UninstallDisplayIcon={app}\quintess.exe
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes
ChangesEnvironment=yes

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked
Name: "openwith"; Description: "Offer Quintess for opening PDF files (choose it as the default in Windows Settings if you like)"
Name: "addtopath"; Description: "Add the quintess command to the PATH, so it can be typed in a terminal"

[InstallDelete]
; Before 0.3.0 the app was a separate quintess-app.exe; quintess.exe is now both.
Type: files; Name: "{app}\quintess-app.exe"

[Files]
Source: "{#BuildFolder}\quintess.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildFolder}\quintess-console.exe"; DestDir: "{app}"; DestName: "quintess.com"; Flags: ignoreversion
Source: "{#PdfiumFolder}\bin\pdfium.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ModelsFolder}\text-detection.onnx"; DestDir: "{app}\models"; Flags: ignoreversion
Source: "{#ModelsFolder}\text-recognition.onnx"; DestDir: "{app}\models"; Flags: ignoreversion
; Spelling dictionaries (US English is also built into the program).
Source: "..\assets\dictionaries\*.aff"; DestDir: "{app}\dictionaries"; Flags: ignoreversion
Source: "..\assets\dictionaries\*.dic"; DestDir: "{app}\dictionaries"; Flags: ignoreversion
Source: "..\assets\dictionaries\*.license.txt"; DestDir: "{app}\licenses\dictionaries"
Source: "..\LICENSE-MIT"; DestDir: "{app}\licenses"
Source: "..\LICENSE-APACHE"; DestDir: "{app}\licenses"
Source: "..\licenses\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}\licenses"
; PDFium's own licenses, and the MIT license of the scripts that built the binary.
Source: "{#PdfiumFolder}\licenses\*"; DestDir: "{app}\licenses\pdfium"
Source: "{#PdfiumFolder}\LICENSE"; DestDir: "{app}\licenses\pdfium"; DestName: "pdfium-binaries-build-scripts.txt"

[Icons]
; Upgrades replace the shortcuts of earlier versions (same names) with these.
Name: "{group}\{#AppName}"; Filename: "{app}\quintess.exe"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\quintess.exe"; Tasks: desktopicon

[Registry]
; A document type for PDFs opened in Quintess.
Root: HKA; Subkey: "Software\Classes\Quintess.Document"; ValueType: string; ValueData: "PDF Document"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Quintess.Document\DefaultIcon"; ValueType: string; ValueData: """{app}\quintess.exe"",0"; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Quintess.Document\shell\open\command"; ValueType: string; ValueData: """{app}\quintess.exe"" ""%1"""; Tasks: openwith
; Listed under "Open with" for .pdf, without taking over the current default.
Root: HKA; Subkey: "Software\Classes\.pdf\OpenWithProgids"; ValueType: string; ValueName: "Quintess.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: openwith
; The "Open with" entry of quintess-app.exe, from before 0.3.0, goes.
Root: HKA; Subkey: "Software\Classes\Applications\quintess-app.exe"; ValueType: none; Flags: deletekey
Root: HKA; Subkey: "Software\Classes\Applications\quintess.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "Quintess"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\quintess.exe\SupportedTypes"; ValueType: string; ValueName: ".pdf"; ValueData: ""; Tasks: openwith
; So Windows Settings can offer Quintess as the default PDF app.
Root: HKA; Subkey: "Software\Quintess\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "Quintess"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Quintess\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Read, edit, sign and redact PDF files."; Tasks: openwith
Root: HKA; Subkey: "Software\Quintess\Capabilities\FileAssociations"; ValueType: string; ValueName: ".pdf"; ValueData: "Quintess.Document"; Tasks: openwith
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "Quintess"; ValueData: "Software\Quintess\Capabilities"; Flags: uninsdeletevalue; Tasks: openwith
; The program folder on the user's PATH (per user, also for an install for
; everyone), so `quintess` works in a terminal; taken off when uninstalling.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Check: NeedsAddPath(ExpandConstant('{app}')); Tasks: addtopath

[Run]
Filename: "{app}\quintess.exe"; Description: "Open Quintess"; Flags: nowait postinstall skipifsilent

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

{ Takes the folder back off the PATH when uninstalling. Only the whole entry
  is removed (not the start of a longer one such as "...\Quintess Tools"),
  wherever it is in the list. }
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
  { Semicolons at both ends, so the first and last entries match like the rest. }
  Paths := ';' + Paths + ';';
  Position := Pos(';' + Uppercase(Folder) + ';', Uppercase(Paths));
  if Position = 0 then
    exit;
  Delete(Paths, Position, Length(Folder) + 1);
  Paths := Copy(Paths, 2, Length(Paths) - 2);
  RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths);
end;
