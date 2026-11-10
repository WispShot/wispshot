; WispShot installer (Inno Setup 6). Per-user install, no admin rights needed.
; Build: iscc /DAppVersion=0.2.0 /DSrcDir=..\target\release installer\wispshot.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SrcDir
  #define SrcDir "..\target\release"
#endif

[Setup]
AppId={{77F8AD58-347E-4055-B17E-2F7BA822D419}
AppName=WispShot
AppVersion={#AppVersion}
AppVerName=WispShot {#AppVersion}
AppPublisher=Efe Kaputlu
AppPublisherURL=https://github.com/efekaputlu/wispshot
AppSupportURL=https://github.com/efekaputlu/wispshot/issues
AppUpdatesURL=https://github.com/efekaputlu/wispshot/releases
DefaultDirName={autopf}\WispShot
PrivilegesRequired=lowest
DisableProgramGroupPage=yes
DisableDirPage=auto
DisableReadyPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
SetupIconFile=..\assets\wispshot.ico
UninstallDisplayIcon={app}\wispshot.exe
UninstallDisplayName=WispShot
WizardStyle=modern
WizardSizePercent=100
Compression=lzma2/max
SolidCompression=yes
OutputDir=..\dist
OutputBaseFilename=WispShot-{#AppVersion}-setup-x64
VersionInfoVersion={#AppVersion}
VersionInfoProductName=WispShot
CloseApplications=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "tr"; MessagesFile: "compiler:Languages\Turkish.isl"

[CustomMessages]
en.Autostart=Start WispShot with Windows
tr.Autostart=WispShot'ı Windows ile başlat
en.Launch=Start WispShot now
tr.Launch=WispShot'ı şimdi başlat

[Tasks]
Name: "autostart"; Description: "{cm:Autostart}"

[Files]
Source: "{#SrcDir}\wispshot.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\WispShot"; Filename: "{app}\wispshot.exe"

[Registry]
; Same value the app writes itself from the tray menu ("Start with Windows"). Uninstall removes it
; only while it still points to this install (CurUninstallStepChanged).
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "WispShot"; ValueData: """{app}\wispshot.exe"""; Tasks: autostart

[Run]
Filename: "{app}\wispshot.exe"; Description: "{cm:Launch}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: files; Name: "{app}\wispshot.ini"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';

{ True while a wispshot.exe started from this install folder is running. A copy running from
  anywhere else (portable, dev build) is left alone. }
function OurCopyRunning(): Boolean;
var
  Locator, Wmi, Procs: Variant;
  Exe: String;
begin
  Result := False;
  Exe := ExpandConstant('{app}\wispshot.exe');
  StringChangeEx(Exe, '\', '\\', True);
  StringChangeEx(Exe, '''', '\''', True);
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Wmi := Locator.ConnectServer('.', 'root\CIMV2');
    Procs := Wmi.ExecQuery('SELECT ProcessId FROM Win32_Process WHERE ExecutablePath = ''' + Exe + '''');
    Result := Procs.Count > 0;
  except
  end;
end;

{ Ask our running copy to quit (the tray app removes its icon itself), so the exe can be replaced or removed.
  "--exit" goes to the running instance, so it is only sent when that instance is ours. }
procedure StopWispShot();
var
  Code, I: Integer;
begin
  if not OurCopyRunning() then
    Exit;
  Exec(ExpandConstant('{app}\wispshot.exe'), '--exit', '', SW_HIDE, ewWaitUntilTerminated, Code);
  for I := 1 to 30 do
  begin
    if not OurCopyRunning() then
      Exit;
    Sleep(100);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Value: String;
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  if RegQueryStringValue(HKCU, RunKey, 'WispShot', Value) then
    if Pos(Lowercase(ExpandConstant('{app}\wispshot.exe')), Lowercase(Value)) > 0 then
      RegDeleteValue(HKCU, RunKey, 'WispShot');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopWispShot();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  StopWispShot();
  Result := True;
end;
