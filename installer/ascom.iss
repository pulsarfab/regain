#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef Stage
  #error Stage is required
#endif
#ifndef Output
  #error Output is required
#endif
#define AppGuid "{6C6E7298-5281-4CB4-92F0-0C3702B8BFAA}"

[Setup]
AppId={{6C6E7298-5281-4CB4-92F0-0C3702B8BFAA}
AppName=ZWOgain ASCOM
AppVersion={#AppVersion}
AppPublisher=StackFoundry LLC
AppPublisherURL=https://github.com/theatrus/zwogain
AppSupportURL=https://github.com/theatrus/zwogain/issues
DefaultDirName={autopf}\ZWOgain ASCOM
DefaultGroupName=ZWOgain ASCOM
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#Output}
OutputBaseFilename=ZwoGain-ASCOM-{#AppVersion}-win-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
LicenseFile={#Stage}\LICENSE
InfoBeforeFile=before-install.txt
UninstallDisplayIcon={app}\ZwoGain.ASCOM.Register.exe
SetupLogging=yes
CloseApplications=no
RestartApplications=no
DisableDirPage=auto
#ifdef SignedBuild
SignTool=azure
SignedUninstaller=yes
#endif

[Files]
Source: "{#Stage}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
; A separate helper checks the old files without loading the installed COM DLL.
Source: "{#Stage}\*.dll"; DestDir: "{tmp}\preflight"; Flags: dontcopy
Source: "{#Stage}\ZwoGain.ASCOM.Register.exe*"; DestDir: "{tmp}\preflight"; Flags: dontcopy

[Registry]
#include RegistryEntries

[Icons]
Name: "{group}\Camera setup"; Filename: "{app}\ZwoGain.ASCOM.Register.exe"
Name: "{group}\CAA rotator setup"; Filename: "{app}\ZwoGain.ASCOM.Register.exe"; Parameters: "/rotator"
Name: "{group}\EFW filter wheel setup"; Filename: "{app}\ZwoGain.ASCOM.Register.exe"; Parameters: "/filterwheel"
Name: "{group}\EAF focuser setup"; Filename: "{app}\ZwoGain.ASCOM.Register.exe"; Parameters: "/focuser"
Name: "{group}\Documentation"; Filename: "https://github.com/theatrus/zwogain/blob/main/docs/ascom.md"

[Code]
const
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#AppGuid}_is1';

function PrerequisiteError: String;
var
  Release: Cardinal;
  Platform: String;
begin
  Result := '';
  if not RegQueryDWordValue(HKLM32, 'SOFTWARE\Microsoft\NET Framework Setup\NDP\v4\Full', 'Release', Release) or (Release < 528040) then
    Result := 'Install Microsoft .NET Framework 4.8 or later, then run setup again. https://dotnet.microsoft.com/download/dotnet-framework/net48'
  else if not RegQueryStringValue(HKLM32, 'SOFTWARE\ASCOM', 'PlatformVersion', Platform) or (CompareText(Platform, '') = 0) then
    Result := 'Install ASCOM Platform, then run setup again. https://ascom-standards.org/Downloads/Index.htm';
end;

function InitializeSetup: Boolean;
var
  Problem, InstalledVersion: String;
  OldVersion, NewVersion: Int64;
begin
  Problem := PrerequisiteError;
  if (Problem = '') and RegQueryStringValue(HKLM64, UninstallKey, 'DisplayVersion', InstalledVersion) then
    if StrToVersion(InstalledVersion, OldVersion) and StrToVersion('{#AppVersion}', NewVersion) then
      if ComparePackedVersion(OldVersion, NewVersion) > 0 then
        Problem := 'A newer ZWOgain ASCOM version is installed. Uninstall it before installing an older version. Camera settings will be kept.';
  Result := Problem = '';
  if not Result then SuppressibleMsgBox(Problem, mbError, MB_OK, IDOK);
end;

function CheckInUse(Helper, Directory: String): String;
var
  ExitCode: Integer;
begin
  Result := '';
  if not Exec(Helper, '/checkinuse "' + Directory + '"', '', SW_HIDE, ewWaitUntilTerminated, ExitCode) then
    Result := 'Could not check running camera applications. Setup has made no changes.'
  else if ExitCode = 2 then
    Result := 'Close camera applications and stop the ZWOgain Alpaca server using this installation, then retry. Running processes are listed in %LOCALAPPDATA%\ZwoGain\ASCOM\registration.log.'
  else if ExitCode <> 0 then
    Result := 'Could not check running camera applications. See %LOCALAPPDATA%\ZwoGain\ASCOM\registration.log.';
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  InstalledPath: String;
begin
  Result := PrerequisiteError;
  if Result <> '' then Exit;
  if RegQueryStringValue(HKLM64, UninstallKey, 'Inno Setup: App Path', InstalledPath) then
    if CompareText(RemoveBackslashUnlessRoot(InstalledPath), RemoveBackslashUnlessRoot(ExpandConstant('{app}'))) <> 0 then begin
      Result := 'Upgrade in the existing installation folder, or uninstall the old copy first. Camera settings will be kept.';
      Exit;
    end;
  ExtractTemporaryFiles('{tmp}\preflight\*');
  Result := CheckInUse(ExpandConstant('{tmp}\preflight\ZwoGain.ASCOM.Register.exe'), ExpandConstant('{app}'));
end;

function CameraCodeBase(Param: String): String;
var
  Path: String;
begin
  Path := ExpandConstant('{app}\ZwoGain.ASCOM.dll');
  StringChangeEx(Path, '\', '/', True);
  { Match RegAsm: the CLR COM loader expects literal spaces in CodeBase. }
  Result := 'file:///' + Path;
end;

function InitializeUninstall: Boolean;
var
  Problem, CodeBase, Key: String;
  Slot, View: Integer;
  Root: Integer;
begin
  Problem := CheckInUse(ExpandConstant('{app}\ZwoGain.ASCOM.Register.exe'), ExpandConstant('{app}'));
  for View := 0 to 1 do begin
    if View = 0 then Root := HKLM32 else Root := HKLM64;
    for Slot := 1 to 4 do begin
      Key := 'Software\Classes\CLSID\{D1DB6F94-5CC0-4752-A758-F849098874A' + IntToStr(Slot) + '}\InprocServer32';
      if RegQueryStringValue(Root, Key, 'CodeBase', CodeBase) then
        if CompareText(CodeBase, CameraCodeBase('')) <> 0 then
          Problem := 'Another ZWOgain copy now owns the camera registration. Restore registration to this installation before uninstalling it.';
    end;
    Key := 'Software\Classes\CLSID\{A918164B-49DD-4FF5-BEE6-A4AB93B97F12}\InprocServer32';
    if RegQueryStringValue(Root, Key, 'CodeBase', CodeBase) then
      if CompareText(CodeBase, CameraCodeBase('')) <> 0 then
        Problem := 'Another ZWOgain copy now owns the rotator registration. Restore registration to this installation before uninstalling it.';
  end;
  Result := Problem = '';
  if not Result then SuppressibleMsgBox(Problem, mbError, MB_OK, IDOK);
end;
