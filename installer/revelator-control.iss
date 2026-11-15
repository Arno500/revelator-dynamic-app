; Inno Setup 6 script for Revelator Control: one installer for x64 and ARM64 Windows.
;
; Built by CI (.github/workflows/ci.yml). To build it by hand, put the release binaries in
; X64Dir and Arm64Dir (each holding revelator-gui.exe and revelator-cli.exe), then:
;   iscc /DAppVersion=0.1.0 /DNumericVersion=0.1.0 /DX64Dir=... /DArm64Dir=... revelator-control.iss
;
; Installs per user (no admin, %LOCALAPPDATA%\Programs) or for all users (Program Files); the
; first page lets the user choose. Settings and presets in %APPDATA%\revelator-control are
; kept on uninstall.

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.0.0"
#endif
#ifndef X64Dir
  #define X64Dir "..\target\x86_64-pc-windows-msvc\release"
#endif
#ifndef Arm64Dir
  #define Arm64Dir "..\target\aarch64-pc-windows-msvc\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

#define AppName "Revelator Control"
#define AppExe "revelator-gui.exe"
; Must match NAME in crates/revelator-gui/src/autostart.rs.
#define RunValue "Revelator Control"

[Setup]
; Never change AppId: Windows uses it to recognise upgrades of the same app.
AppId={{ED018B61-5496-4035-BAFC-799619994AF0}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Arno
AppPublisherURL=https://github.com/Arno500/revelator-dynamic-app
AppSupportURL=https://github.com/Arno500/revelator-dynamic-app/issues
AppUpdatesURL=https://github.com/Arno500/revelator-dynamic-app/releases
VersionInfoVersion={#NumericVersion}
VersionInfoProductVersion={#NumericVersion}
VersionInfoDescription={#AppName} Setup
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; x64compatible covers x64 and Windows 11 on ARM; arm64 adds Windows 10 on ARM.
ArchitecturesAllowed=x64compatible or arm64
ArchitecturesInstallIn64BitMode=x64compatible or arm64
MinVersion=10.0
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=no
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
#ifdef IconFile
SetupIconFile={#IconFile}
#endif
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename=revelator-control-{#AppVersion}-setup

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"

[CustomMessages]
english.AddToPath=Add revelator-cli to PATH (for the one-time setup command)
french.AddToPath=Ajouter revelator-cli au PATH (pour la commande de configuration initiale)

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "{cm:AddToPath}"

[Files]
Source: "{#X64Dir}\revelator-gui.exe"; DestDir: "{app}"; Check: not IsArm64; Flags: ignoreversion
Source: "{#X64Dir}\revelator-cli.exe"; DestDir: "{app}"; Check: not IsArm64; Flags: ignoreversion
Source: "{#Arm64Dir}\revelator-gui.exe"; DestDir: "{app}"; Check: IsArm64; Flags: ignoreversion
Source: "{#Arm64Dir}\revelator-cli.exe"; DestDir: "{app}"; Check: IsArm64; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[Code]
const
  UserEnvKey = 'Environment';
  SystemEnvKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  ApprovedKey = 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run';

{ PATH of the scope this install uses: the machine's for all users, else the user's. }
procedure EnvKey(var Root: Integer; var Key: String);
begin
  if IsAdminInstallMode then
  begin
    Root := HKEY_LOCAL_MACHINE;
    Key := SystemEnvKey;
  end
  else
  begin
    Root := HKEY_CURRENT_USER;
    Key := UserEnvKey;
  end;
end;

function PathHas(Path, Dir: String): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') > 0;
end;

procedure AddToPath(Dir: String);
var
  Root: Integer;
  Key, Path: String;
begin
  EnvKey(Root, Key);
  if not RegQueryStringValue(Root, Key, 'Path', Path) then
    Path := '';
  if PathHas(Path, Dir) then
    exit;
  { Keep the existing style: a PATH that ended with ';' still does, so removal restores it. }
  if Path = '' then
    Path := Dir
  else if Copy(Path, Length(Path), 1) = ';' then
    Path := Path + Dir + ';'
  else
    Path := Path + ';' + Dir;
  RegWriteExpandStringValue(Root, Key, 'Path', Path);
end;

procedure RemoveFromPath(Dir: String);
var
  Root: Integer;
  Key, Path: String;
begin
  EnvKey(Root, Key);
  if not RegQueryStringValue(Root, Key, 'Path', Path) then
    exit;
  if not PathHas(Path, Dir) then
    exit;
  Path := ';' + Path + ';';
  StringChangeEx(Path, ';' + Dir + ';', ';', True);
  { Drop only the two separators added above; the rest of PATH is left exactly as it was. }
  Delete(Path, 1, 1);
  if Length(Path) > 0 then
    Delete(Path, Length(Path), 1);
  RegWriteExpandStringValue(Root, Key, 'Path', Path);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    AddToPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Cmd: String;
begin
  if CurUninstallStep <> usPostUninstall then
    exit;
  RemoveFromPath(ExpandConstant('{app}'));
  { The app's own "Start on boot" entry (per user): drop it if it points at this install. }
  if RegQueryStringValue(HKEY_CURRENT_USER, RunKey, '{#RunValue}', Cmd) and
     (Pos(Uppercase(ExpandConstant('{app}')), Uppercase(Cmd)) > 0) then
  begin
    RegDeleteValue(HKEY_CURRENT_USER, RunKey, '{#RunValue}');
    RegDeleteValue(HKEY_CURRENT_USER, ApprovedKey, '{#RunValue}');
  end;
end;
