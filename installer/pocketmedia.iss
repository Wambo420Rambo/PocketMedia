; PocketMedia installer and uninstaller.
;
; Compile with Inno Setup 6.3 or newer (ISCC.exe), after `cargo build --release`.
;
;   ISCC installer\pocketmedia.iss                      -> installer\Output\PocketMedia-Setup-<version>.exe
;   ISCC /DTestBuild /DAppVersion=x installer\pocketmedia.iss
;        a copy with its own AppId, for throwaway test installs only
;
; Decisions:
; - No elevation (PrivilegesRequired=lowest). PocketMedia writes its Media, Data and
;   Cache beside its exe, so it must live in a folder the user can write to anyway.
; - Setup refuses to run as an account other than the one signed in at the screen
;   ("Run as administrator" with an administrator's password would
;   otherwise give that account the folder, the profile and Autostart).
; - The installer hands its choices to the app with the smallest possible step: on a
;   first install with no library yet, it writes Data\pocketmedia.json holding only
;   the chosen profile; Autostart is the HKCU Run value the app already treats as the
;   truth. An existing library file is never written, so upgrades keep everything.
; - The uninstaller asks for the exact text CONFIRM, refuses /SILENT and /VERYSILENT,
;   and removes only Media, Data, Cache, the Run value that starts this copy, and the
;   files Setup installed. It does not follow junctions or symbolic links.

#if VER < EncodeVer(6,3,0)
  #error Inno Setup 6.3 or newer is needed (for ArchitecturesAllowed=x64os).
#endif

#ifndef AppVersion
  ; Keep in step with Cargo.toml.
  #define AppVersion "1.0.0"
#endif

#ifdef TestBuild
  ; A separate identity, so a test install can never be taken for (or upgrade)
  ; a real PocketMedia install.
  #define AppId "PocketMedia.InstallerTest"
  #define AppName "PocketMedia installer test"
#else
  #define AppId "PocketMedia"
  #define AppName "PocketMedia"
#endif

#define ExeName "pocketmedia.exe"
#define ExeSource AddBackslash(SourcePath) + "..\target\release\" + ExeName
#if !FileExists(ExeSource)
  #error target\release\pocketmedia.exe is missing. Run cargo build --release first.
#endif

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Wambo420Rambo
AppPublisherURL=https://github.com/Wambo420Rambo/PocketMedia
AppSupportURL=https://github.com/Wambo420Rambo/PocketMedia/issues
AppUpdatesURL=https://github.com/Wambo420Rambo/PocketMedia/releases
VersionInfoVersion=1.0.0.0
DefaultDirName=C:\PocketMedia
UsePreviousAppDir=yes
DirExistsWarning=no
; Our own first page holds the folder (one page for behavior, Autostart and folder).
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableWelcomePage=yes
; The license (IDGAFPL), shown before our own first page.
LicenseFile=..\LICENSE.md
DisableReadyPage=no
DisableFinishedPage=no
PrivilegesRequired=lowest
; Windows 11 (build 22000 or later), Intel/AMD x64 only. x64os excludes Arm64,
; which would otherwise run x64 programs under emulation.
MinVersion=10.0.22000
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
; Every PocketMedia holds this mutex while it runs (instance.rs). Setup and Uninstall
; wait until it is closed. It is the same for every install, because Setup checks it
; before it knows the folder; each install also holds its own Local\PocketMedia.<id>,
; and only one PocketMedia runs at a time anyway.
AppMutex=Local\PocketMedia.SingleInstance
CloseApplications=no
#if VER >= EncodeVer(6,6,0)
WizardStyle=modern dark
#else
WizardStyle=modern
#endif
; The logo (assets\make-icon.py makes these from the same drawing as the app's icon).
SetupIconFile=..\assets\pocketmedia.ico
WizardSmallImageFile=..\assets\wizard-small-100.bmp,..\assets\wizard-small-125.bmp,..\assets\wizard-small-150.bmp,..\assets\wizard-small-200.bmp
WizardImageAlphaFormat=defined
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\{#ExeName}
SetupLogging=yes
OutputBaseFilename=PocketMedia-Setup-{#AppVersion}
OutputDir=Output
Compression=lzma2
SolidCompression=yes

[Messages]
WindowsVersionNotSupported=PocketMedia runs only on Windows 11 with an Intel or AMD 64-bit (x64) processor. Windows 10, Arm64 and 32-bit Windows are not supported.
WinVersionTooLowError=PocketMedia needs Windows 11 (build 22000 or later).
ConfirmUninstall=Last check: remove %1 and permanently delete its Media folder now?

[Dirs]
; Never removed by Inno itself; the uninstaller code removes them after CONFIRM.
Name: "{app}\Media"; Flags: uninsneveruninstall
Name: "{app}\Media\Pictures"; Flags: uninsneveruninstall
Name: "{app}\Media\GIFs"; Flags: uninsneveruninstall
Name: "{app}\Media\Videos"; Flags: uninsneveruninstall
Name: "{app}\Data"; Flags: uninsneveruninstall
Name: "{app}\Cache"; Flags: uninsneveruninstall

[Files]
Source: "{#ExeSource}"; DestDir: "{app}"; Flags: ignoreversion

#ifndef TestBuild
[Icons]
Name: "{autoprograms}\PocketMedia"; Filename: "{app}\{#ExeName}"
#endif

[Run]
Filename: "{app}\{#ExeName}"; Description: "Start PocketMedia"; Flags: postinstall nowait skipifsilent

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  RunValue = 'PocketMedia';
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#AppId}_is1';
  ExeName = '{#ExeName}';

var
  { Setup }
  IsUpgrade: Boolean;
  PrevDir: String;
  ChoicePage: TWizardPage;
  LessRadio, FasterRadio: TNewRadioButton;
  AutostartBox: TNewCheckBox;
  PathText: TNewStaticText;
  AppDirExisted: Boolean;
  { Uninstall }
  ConfirmEdit: TNewEdit;
  ConfirmButton: TNewButton;
  BackupNote: TNewStaticText;
  RemoveProblems: String;

{ ---------- shared ---------- }

function AutostartCommand(const App: String): String;
begin
  { The exact text autostart.rs writes and compares (case-insensitively). }
  Result := '"' + AddBackslash(App) + ExeName + '" --autostart';
end;

function HasLibrary(const Dir: String): Boolean;
begin
  Result := FileExists(AddBackslash(Dir) + 'Data\pocketmedia.json') or
            FileExists(AddBackslash(Dir) + 'Data\pocketmedia.json.bak');
end;

function IsPocketMediaFolder(const Dir: String): Boolean;
begin
  Result := FileExists(AddBackslash(Dir) + ExeName) or HasLibrary(Dir);
end;

function StartsWithFolder(const Path, Folder: String): Boolean;
var
  F: String;
begin
  F := AddBackslash(Folder);
  Result := (Folder <> '') and (CompareText(Copy(AddBackslash(Path), 1, Length(F)), F) = 0);
end;

{ ---------- Setup: choices ---------- }

{ 'less', 'faster', or '' when not chosen yet. }
function ChosenProfile: String;
begin
  Result := '';
  if WizardSilent then begin
    Result := Lowercase(ExpandConstant('{param:PROFILE|}'));
    if (Result <> 'less') and (Result <> 'faster') then
      Result := '';
  end
  else if LessRadio.Checked then
    Result := 'less'
  else if FasterRadio.Checked then
    Result := 'faster';
end;

function ProfileTitle(const P: String): String;
begin
  if P = 'faster' then Result := 'Open faster' else Result := 'Use less memory';
end;

function AutostartChosen: Boolean;
begin
  if WizardSilent then
    Result := Lowercase(ExpandConstant('{param:AUTOSTART|no}')) = 'yes'
  else
    Result := AutostartBox.Checked;
end;

{ Why Dir cannot hold PocketMedia, or '' when it can. Nothing is created. }
function FolderProblem(const Dir: String): String;
var
  Probe, Parent: String;
begin
  Result := '';
  if (Length(Dir) < 3) or (Dir[2] <> ':') or (Dir[3] <> '\') then
    Result := 'Choose a folder on a drive of this computer, such as C:\PocketMedia.'
  else if Length(RemoveBackslashUnlessRoot(Dir)) <= 3 then
    Result := 'Choose a folder, not a whole drive. PocketMedia keeps its Media, Data and Cache folders beside the program, and uninstalling deletes them.'
  else if StartsWithFolder(Dir, ExpandConstant('{win}')) or
          StartsWithFolder(Dir, ExpandConstant('{commonpf64}')) or
          StartsWithFolder(Dir, ExpandConstant('{commonpf32}')) then
    Result := 'PocketMedia keeps your media beside the program, so it cannot go in Program Files or Windows. Choose a folder such as C:\PocketMedia.'
  else if (DirExists(AddBackslash(Dir) + 'Media') or DirExists(AddBackslash(Dir) + 'Data') or
           DirExists(AddBackslash(Dir) + 'Cache')) and not IsPocketMediaFolder(Dir) then
    Result := 'This folder already has a Media, Data or Cache folder that does not belong to PocketMedia. ' +
              'Uninstalling PocketMedia would delete it, so choose another folder.'
  else begin
    { Can the user create folders here? Try in the nearest folder that exists. }
    Parent := RemoveBackslashUnlessRoot(Dir);
    while not DirExists(Parent) and (Length(Parent) > 3) do
      Parent := ExtractFileDir(Parent);
    Probe := AddBackslash(Parent) + 'pocketmedia-setup-check-' + GetDateTimeString('yyyymmddhhnnsszzz', #0, #0);
    if CreateDir(Probe) then
      RemoveDir(Probe)
    else
      Result := 'You cannot create files in this folder. Choose a folder you can write to, such as C:\PocketMedia.';
  end;
end;

{ Browse gives the parent; PocketMedia gets its own folder in it, unless the
  chosen folder already is one. }
function WithAppFolder(Dir: String): String;
begin
  Dir := RemoveBackslashUnlessRoot(Dir);
  if (CompareText(ExtractFileName(Dir), 'PocketMedia') = 0) or IsPocketMediaFolder(Dir) then
    Result := Dir
  else
    Result := AddBackslash(Dir) + 'PocketMedia';
end;

procedure UpdateNextButton;
begin
  if (ChoicePage <> nil) and not WizardSilent and (WizardForm.CurPageID = ChoicePage.ID) then
    WizardForm.NextButton.Enabled := LessRadio.Checked or FasterRadio.Checked;
end;

procedure ProfileClick(Sender: TObject);
begin
  UpdateNextButton;
end;

procedure BrowseClick(Sender: TObject);
var
  Dir: String;
begin
  Dir := WizardForm.DirEdit.Text;
  if BrowseForFolder('Choose where to install PocketMedia. Its Media, Data and Cache folders go there too.', Dir, False) then begin
    Dir := WithAppFolder(Dir);
    WizardForm.DirEdit.Text := Dir;
    PathText.Caption := Dir;
  end;
end;

function AddText(const Parent: TWinControl; const Caption: String; const Left, Top, Width: Integer): TNewStaticText;
begin
  Result := TNewStaticText.Create(ChoicePage);
  Result.Parent := Parent;
  Result.Left := Left;
  Result.Top := Top;
  Result.Width := Width;
  Result.AutoSize := False;
  Result.WordWrap := True;
  Result.ShowAccelChar := False;
  Result.Caption := Caption;
  Result.AdjustHeight;
end;

procedure AddProfileCard(const Radio: TNewRadioButton; const Caption, Description: String; const Left, Width: Integer);
var
  Frame: TBevel;
begin
  Frame := TBevel.Create(ChoicePage);
  Frame.Parent := ChoicePage.Surface;
  Frame.Shape := bsFrame;
  Frame.SetBounds(Left, 0, Width, ScaleY(78));
  Radio.Parent := ChoicePage.Surface;
  Radio.Caption := Caption;
  Radio.SetBounds(Left + ScaleX(10), ScaleY(10), Width - ScaleX(20), ScaleY(18));
  Radio.Checked := False;
  Radio.OnClick := @ProfileClick;
  AddText(ChoicePage.Surface, Description, Left + ScaleX(28), ScaleY(32), Width - ScaleX(38));
end;

procedure CreateChoicePage;
var
  W, Half, Y: Integer;
  T: TNewStaticText;
  Browse: TNewButton;
begin
  ChoicePage := CreateCustomPage(wpLicense, 'Choose how PocketMedia behaves',
    'Choose an option to continue. You can change it later in Settings.');
  W := ChoicePage.SurfaceWidth;
  Half := (W - ScaleX(8)) div 2;

  LessRadio := TNewRadioButton.Create(ChoicePage);
  FasterRadio := TNewRadioButton.Create(ChoicePage);
  AddProfileCard(LessRadio, 'Use less memory',
    'Release unused windows and video players. Opening a window may take a moment.', 0, Half);
  AddProfileCard(FasterRadio, 'Open faster',
    'Keep the quick popup ready. Uses more memory while idle.', W - Half, Half);

  Y := ScaleY(90);
  AutostartBox := TNewCheckBox.Create(ChoicePage);
  AutostartBox.Parent := ChoicePage.Surface;
  AutostartBox.Caption := 'Autostart PocketMedia';
  AutostartBox.SetBounds(0, Y, W, ScaleY(18));
  AutostartBox.Checked := False;

  Y := Y + ScaleY(28);
  T := AddText(ChoicePage.Surface,
    'GIFs loop while visible. Video previews play on hover and can be switched off in Settings.', 0, Y, W);

  Y := T.Top + T.Height + ScaleY(16);
  T := AddText(ChoicePage.Surface, 'Installation folder', 0, Y, W);
  T.Font.Style := [fsBold];

  Y := T.Top + T.Height + ScaleY(8);
  Browse := TNewButton.Create(ChoicePage);
  Browse.Parent := ChoicePage.Surface;
  Browse.Caption := 'Browse…';
  Browse.SetBounds(W - ScaleX(90), Y, ScaleX(90), ScaleY(23));
  Browse.OnClick := @BrowseClick;

  PathText := AddText(ChoicePage.Surface, WizardForm.DirEdit.Text, 0, Y + ScaleY(4), W - ScaleX(100));
end;

{ ---------- Setup: the right account ---------- }

{ The account signed in at the screen (Win32_ComputerSystem.UserName, "DOMAIN\user"),
  or '' when Windows does not say (for example over Remote Desktop). The test build
  can pretend another account is signed in, to check the refusal. }
function ScreenUser: String;
var
  Locator, Service, Systems, Item: Variant;
  I: Integer;
begin
  Result := '';
#ifdef TestBuild
  Result := ExpandConstant('{param:TESTSCREENUSER|}');
  if Result <> '' then
    Exit;
#endif
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Locator.ConnectServer('.', 'root\CIMV2');
    Systems := Service.ExecQuery('SELECT UserName FROM Win32_ComputerSystem');
    for I := 0 to Systems.Count - 1 do begin
      Item := Systems.ItemIndex(I);
      if not VarIsNull(Item.UserName) then
        Result := Item.UserName;
    end;
  except
    Log('Could not ask Windows who is signed in: ' + GetExceptionMessage);
    Result := '';
  end;
end;

{ The part after the backslash: "PC\Anna" -> "Anna". }
function AccountName(const User: String): String;
var
  P: Integer;
begin
  Result := User;
  P := Pos('\', Result);
  while P > 0 do begin
    Delete(Result, 1, P);
    P := Pos('\', Result);
  end;
end;

{ '' when Setup runs as the account at the screen (or Windows does not say who that
  is); otherwise the message that refuses. }
function WrongAccount: String;
var
  Screen: String;
begin
  Result := '';
  Screen := ScreenUser;
  if Screen = '' then begin
    Log('Windows did not say who is signed in at the screen; the account was not compared.');
    Exit;
  end;
  if CompareText(AccountName(Screen), GetUserNameString) = 0 then
    Log('Setup runs as ' + GetUserNameString + ', the account signed in at the screen (' + Screen + ').')
  else
    Result := 'Setup is running as ' + GetUserNameString + ', but ' + AccountName(Screen) + ' is signed in. ' +
      'PocketMedia installs for one account, and its media, settings and Autostart belong to that account.' + #13#10#13#10 +
      'Start Setup again as ' + AccountName(Screen) + ', without "Run as administrator". It needs no administrator rights.';
end;

{ ---------- Setup: event functions ---------- }

function InitializeSetup: Boolean;
var
  Problem: String;
begin
  Result := True;
  Problem := WrongAccount;
  if Problem <> '' then begin
    Log('Refused: ' + Problem);
    SuppressibleMsgBox(Problem, mbError, MB_OK, IDOK);
    Result := False;
    Exit;
  end;
  PrevDir := '';
  if RegQueryStringValue(HKEY_CURRENT_USER, UninstallKey, 'Inno Setup: App Path', PrevDir) then
    PrevDir := RemoveBackslashUnlessRoot(PrevDir);
  IsUpgrade := (PrevDir <> '') and FileExists(AddBackslash(PrevDir) + ExeName);
  if IsUpgrade then
    Log('Upgrade of the PocketMedia in ' + PrevDir + '; media, data and settings are kept.')
  else if WizardSilent and (ChosenProfile = '') then begin
    { The profile must be chosen; a silent install has to name it. }
    Log('Refused: a silent first install needs /PROFILE=less or /PROFILE=faster.');
    SuppressibleMsgBox('A silent install of PocketMedia needs /PROFILE=less or /PROFILE=faster.',
      mbError, MB_OK, IDOK);
    Result := False;
  end;
end;

procedure InitializeWizard;
begin
  if IsUpgrade then
    { The library lives beside the program: an upgrade stays where it is. }
    WizardForm.DirEdit.Text := PrevDir;
  CreateChoicePage;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  { A silent install takes its choices from /PROFILE and /AUTOSTART. }
  Result := (PageID = ChoicePage.ID) and (IsUpgrade or WizardSilent);
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  UpdateNextButton;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Problem: String;
begin
  Result := True;
  { A silent install is checked in PrepareToInstall, which also covers this. }
  if (CurPageID = ChoicePage.ID) and not WizardSilent then begin
    Problem := '';
    if ChosenProfile = '' then
      Problem := 'Choose Use less memory or Open faster.'
    else
      Problem := FolderProblem(WizardForm.DirEdit.Text);
    if Problem <> '' then begin
      Log('Next refused: ' + Problem);
      MsgBox(Problem, mbError, MB_OK);
      Result := False;
    end;
  end;
end;

function UpdateReadyMemo(Space, NewLine, MemoUserInfoInfo, MemoDirInfo, MemoTypeInfo,
  MemoComponentsInfo, MemoGroupInfo, MemoTasksInfo: String): String;
var
  Dir: String;
begin
  Dir := WizardForm.DirEdit.Text;
  if IsUpgrade then begin
    Result := 'Upgrade PocketMedia in' + NewLine + Space + Dir + NewLine + NewLine +
      'Kept: all media, the library, settings, history, Autostart and the behavior choice.';
    Exit;
  end;
  Result := 'Behavior:' + Space + ProfileTitle(ChosenProfile) + NewLine;
  if AutostartChosen then
    Result := Result + 'Autostart PocketMedia:' + Space + 'Yes' + NewLine
  else
    Result := Result + 'Autostart PocketMedia:' + Space + 'No' + NewLine;
  Result := Result + 'Video preview:' + Space + 'Hover only' + NewLine +
    'GIFs:' + Space + 'Loop while visible' + NewLine + NewLine +
    'Installation folder:' + NewLine + Space + Dir;
  if HasLibrary(Dir) then
    Result := Result + NewLine + NewLine +
      'This folder already holds a PocketMedia library. It is kept with its own settings; the behavior chosen above is not applied.';
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Dir: String;
begin
  Result := '';
  Dir := RemoveBackslashUnlessRoot(WizardDirValue);
  AppDirExisted := DirExists(Dir);
  if not IsUpgrade then begin
    { Checked again here because a silent install never shows the first page. }
    Result := FolderProblem(Dir);
    if (Result = '') and (ChosenProfile = '') then
      Result := 'Choose Use less memory or Open faster.';
  end;
  if Result <> '' then
    Log('Refused: ' + Result);
end;

{ Only for a folder Setup made itself: drop the permissions inherited from the
  drive (on C:\ these let every signed-in user change files, including
  pocketmedia.exe) and give this user Modify. Other users get no access at all:
  the media is personal. SYSTEM and Administrators keep full control. }
procedure LimitFolderAccess(const App: String);
var
  Code: Integer;
  User, Icacls: String;
begin
  User := GetUserNameString;
  if GetEnv('USERDOMAIN') <> '' then
    User := GetEnv('USERDOMAIN') + '\' + User;
  Icacls := ExpandConstant('{sys}\icacls.exe');
  Code := -1;
  { First grant, and only when that worked drop the inherited entries, so a
    failure never leaves the folder without access for this user. }
  if Exec(Icacls, '"' + App + '" /grant "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" "' +
          User + ':(OI)(CI)M"', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0) then begin
    Code := -1;
    if Exec(Icacls, '"' + App + '" /inheritance:r', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0) then begin
      Log('Folder access limited to ' + User + ' (modify); other users have none.');
      Exit;
    end;
  end;
  begin
    Log('icacls failed (' + IntToStr(Code) + '); the folder keeps the permissions of its parent.');
    SuppressibleMsgBox('PocketMedia is installed, but Windows did not accept the folder permissions ' +
      '(icacls ' + IntToStr(Code) + '). The folder keeps the permissions of the folder it is in.',
      mbInformation, MB_OK, IDOK);
  end;
end;

procedure WriteFirstRunChoices(const App: String);
var
  Json, Profile: String;
begin
  if HasLibrary(App) then begin
    Log('An existing library was found; it is kept and nothing is written to Data.');
    Exit;
  end;
  if ChosenProfile = 'faster' then Profile := 'open_faster' else Profile := 'less_memory';
  Json := '{"version": 1, "next_id": 1, "settings": {"profile": "' + Profile + '"}}' + #13#10;
  if SaveStringToFile(AddBackslash(App) + 'Data\pocketmedia.json', Json, False) then
    Log('Wrote the chosen profile (' + Profile + ') to Data\pocketmedia.json.')
  else
    SuppressibleMsgBox('PocketMedia is installed, but the behavior choice could not be saved. ' +
      'PocketMedia will use "Use less memory"; you can change it in Settings.', mbInformation, MB_OK, IDOK);
end;

{ The program a Run value starts: the quoted path at its start, or its first word
  (as autostart.rs program_of). }
function ProgramOf(Value: String): String;
var
  P: Integer;
begin
  Value := Trim(Value);
  if Copy(Value, 1, 1) = '"' then begin
    Delete(Value, 1, 1);
    P := Pos('"', Value);
  end
  else
    P := Pos(' ', Value);
  if P > 0 then
    Result := Copy(Value, 1, P - 1)
  else
    Result := Value;
end;

procedure TurnOnAutostart(const App: String);
var
  Stored, Other: String;
begin
  { Never take over another PocketMedia's Autostart: only one runs
    at a time. A value whose program is gone is stale and is replaced. }
  if RegQueryStringValue(HKEY_CURRENT_USER, RunKey, RunValue, Stored) and
     (CompareText(Stored, AutostartCommand(App)) <> 0) then begin
    Other := ProgramOf(Stored);
    if (Other <> '') and FileExists(Other) then begin
      Log('Autostart left as it is: it starts another PocketMedia (' + Stored + ').');
      SuppressibleMsgBox('PocketMedia is installed, but Autostart was not turned on: it already starts another PocketMedia, in ' +
        ExtractFileDir(Other) + '. Only one PocketMedia runs at a time. To autostart this one, turn Autostart off in that copy, ' +
        'then turn it on in this one''s Settings.', mbInformation, MB_OK, IDOK);
      Exit;
    end;
  end;
  if RegWriteStringValue(HKEY_CURRENT_USER, RunKey, RunValue, AutostartCommand(App)) and
     RegQueryStringValue(HKEY_CURRENT_USER, RunKey, RunValue, Stored) and
     (CompareText(Stored, AutostartCommand(App)) = 0) then
    Log('Autostart on: ' + Stored)
  else
    SuppressibleMsgBox('PocketMedia is installed, but Windows did not keep the Autostart setting. ' +
      'You can turn it on later in Settings.', mbInformation, MB_OK, IDOK);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  App: String;
begin
  if (CurStep <> ssPostInstall) or IsUpgrade then
    Exit;
  App := ExpandConstant('{app}');
  if not AppDirExisted then
    LimitFolderAccess(App);
  WriteFirstRunChoices(App);
  if AutostartChosen then
    TurnOnAutostart(App);
end;

{ ---------- Uninstall ---------- }

{ Asks this copy which PocketMedia is running (pocketmedia.exe --which-running):
  0 none, 1 this one, 2 a different one (its folder in Other), -1 it could not be asked. }
function WhichRunning(const App: String; var Other: String): Integer;
var
  Code: Integer;
  Info: String;
  Answer: TArrayOfString;
begin
  Other := '';
  Info := ExpandConstant('{tmp}\pocketmedia-running.txt');
  DeleteFile(Info);
  if not Exec(AddBackslash(App) + ExeName, '--which-running "' + Info + '"', App, SW_HIDE, ewWaitUntilTerminated, Code) then begin
    Result := -1;
    Exit;
  end;
  Result := Code;
  if Result = 2 then begin
    { UTF-8 with a BOM, so a folder name in any language reads back right. }
    if LoadStringsFromFile(Info, Answer) and (GetArrayLength(Answer) > 0) and (Pos(#9, Answer[0]) > 0) then
      Other := Trim(Copy(Answer[0], Pos(#9, Answer[0]) + 1, MaxInt));
    if Other = '' then
      Other := 'another folder';
  end
  else if (Result < 0) or (Result > 2) then
    Result := -1;
end;

{ Back up library… on the warning: PocketMedia's own backup (checked copies of
  the whole Media folder, with the library file), started with --backup.
  The note says which PocketMedia the request reached: this copy (already
  running, or started now), a different copy (then nothing is handed over and
  nothing is backed up), or none. It never says the backup
  happened: only PocketMedia knows when it has. }
procedure BackupClick(Sender: TObject);
var
  Code, Running, I: Integer;
  App, Exe, Other, Finished: String;
begin
  Finished := 'Your media is backed up only when PocketMedia shows "Backup finished". ' +
    'Then exit PocketMedia (right-click its tray icon, then Exit) and come back here.';
  App := RemoveBackslashUnlessRoot(ExpandConstant('{app}'));
  Exe := AddBackslash(App) + ExeName;
  Running := WhichRunning(App, Other);
  Log('Which PocketMedia is running: ' + IntToStr(Running) + ' (0 none, 1 this one, 2 a different one, -1 could not ask).');
  if Running = -1 then begin
    BackupNote.Caption := 'Nothing was backed up: PocketMedia could not be started. ' +
      'Copy the Media folder somewhere safe yourself before uninstalling.';
    Exit;
  end;
  if Running = 2 then begin
    Log('Back up library not started: a different PocketMedia is running, from ' + Other + '.');
    BackupNote.Caption := 'Nothing was backed up. A different PocketMedia is running, from ' + Other +
      ', and it cannot back up this one. Exit it (right-click its tray icon, then Exit), then click Back up library… again.';
    Exit;
  end;
  if Running = 1 then begin
    { The running copy is asked; the launch ends as soon as it has handed the request over. }
    if Exec(Exe, '--backup', App, SW_SHOWNORMAL, ewWaitUntilTerminated, Code) and (Code = 0) then begin
      Log('Back up library handed to the running PocketMedia.');
      BackupNote.Caption := 'PocketMedia was running and was asked to open Back up library. ' + Finished;
    end
    else begin
      Log('The running PocketMedia did not take the request (exit code ' + IntToStr(Code) + ').');
      BackupNote.Caption := 'Nothing was backed up: the running PocketMedia did not answer. ' +
        'Open it from its tray icon and choose Settings, then Back up library.';
    end;
    Exit;
  end;
  if not Exec(Exe, '--backup', App, SW_SHOWNORMAL, ewNoWait, Code) then begin
    BackupNote.Caption := 'Nothing was backed up: PocketMedia could not be started (' + SysErrorMessage(Code) + '). ' +
      'Copy the Media folder somewhere safe yourself before uninstalling.';
    Exit;
  end;
  Log('Started Back up library (' + Exe + ' --backup).');
  { Check that it was this copy that started (another one may have started meanwhile). }
  for I := 1 to 40 do begin
    Running := WhichRunning(App, Other);
    if Running <> 0 then
      Break;
    Sleep(250);
  end;
  Log('After starting: ' + IntToStr(Running) + '.');
  case Running of
    1: BackupNote.Caption := 'PocketMedia started and opened Back up library. ' + Finished;
    2: BackupNote.Caption := 'Nothing was backed up. A different PocketMedia is running, from ' + Other +
         ', so this one did not start. Exit that one (right-click its tray icon, then Exit), then click Back up library… again.';
  else
    BackupNote.Caption := 'Nothing was backed up: PocketMedia did not start. ' +
      'Copy the Media folder somewhere safe yourself before uninstalling.';
  end;
end;

procedure ConfirmChange(Sender: TObject);
begin
  { Exact and case-sensitive: "confirm" or "Confirm" keep the button disabled. }
  ConfirmButton.Enabled := ConfirmEdit.Text = 'CONFIRM';
end;

function UninstText(const Form: TSetupForm; const Caption: String; const Left, Top, Width: Integer): TNewStaticText;
begin
  Result := TNewStaticText.Create(Form);
  Result.Parent := Form;
  Result.Left := Left;
  Result.Top := Top;
  Result.Width := Width;
  Result.AutoSize := False;
  Result.WordWrap := True;
  Result.ShowAccelChar := False;
  Result.Caption := Caption;
  Result.AdjustHeight;
end;

{ The permanent-deletion warning. True only after the button was clicked with
  exactly CONFIRM typed; Cancel, Escape or closing the window give False. }
function AskForConfirm: Boolean;
var
  Form: TSetupForm;
  T: TNewStaticText;
  Frame: TBevel;
  Stripe: TBitmapImage;
  CancelBtn, BackupBtn: TNewButton;
  W, X, Y, BoxTop, Res: Integer;
begin
#if VER >= EncodeVer(6,6,0)
  Form := CreateCustomForm(ScaleX(520), ScaleY(450), False, False);
#else
  Form := CreateCustomForm;
  Form.ClientWidth := ScaleX(520);
  Form.ClientHeight := ScaleY(450);
#endif
  try
    Form.Caption := 'PocketMedia Uninstall';
    X := ScaleX(20);
    W := Form.ClientWidth - 2 * X;

    T := UninstText(Form, 'Uninstall PocketMedia', X, ScaleY(16), W);
    T.Font.Size := T.Font.Size + 5;
    T.AdjustHeight;
    T := UninstText(Form, 'Back up your media before continuing.', X, T.Top + T.Height + ScaleY(4), W);

    BoxTop := T.Top + T.Height + ScaleY(14);
    Frame := TBevel.Create(Form);
    Frame.Parent := Form;
    Frame.Shape := bsFrame;

    Y := BoxTop + ScaleY(12);
    T := UninstText(Form, 'Your media will be deleted', X + ScaleX(14), Y, W - ScaleX(28));
    T.Font.Size := T.Font.Size + 3;
    T.Font.Style := [fsBold];
    T.Font.Color := $6A6AF0;
    T.AdjustHeight;
    T := UninstText(Form, 'Uninstalling PocketMedia permanently deletes your Media folder and every file inside:',
      X + ScaleX(14), T.Top + T.Height + ScaleY(8), W - ScaleX(28));
    T := UninstText(Form, AddBackslash(ExpandConstant('{app}')) + 'Media', X + ScaleX(14), T.Top + T.Height + ScaleY(6), W - ScaleX(28));
    T.Font.Style := [fsBold];
    T := UninstText(Form, 'Back up your pictures, GIFs and videos to a safe location before continuing.',
      X + ScaleX(14), T.Top + T.Height + ScaleY(8), W - ScaleX(28));
    T := UninstText(Form, 'Use Back up library below (or in Settings) to save your media outside PocketMedia.',
      X + ScaleX(14), T.Top + T.Height + ScaleY(6), W - ScaleX(28));
    T := UninstText(Form, 'This cannot be undone. Files in the original folders you imported from are kept.',
      X + ScaleX(14), T.Top + T.Height + ScaleY(6), W - ScaleX(28));
    Frame.SetBounds(X, BoxTop, W, T.Top + T.Height + ScaleY(12) - BoxTop);
    { A red bar down the warning's left edge. Inno's dark style recolours text and
      frames, but not pictures, so the warning stays red there too. }
    Stripe := TBitmapImage.Create(Form);
    Stripe.Parent := Form;
    Stripe.Bitmap.Width := 4;
    Stripe.Bitmap.Height := 4;
    Stripe.Bitmap.Canvas.Brush.Color := $6A6AF0;
    Stripe.Bitmap.Canvas.Pen.Color := $6A6AF0;
    Stripe.Bitmap.Canvas.Rectangle(0, 0, 4, 4);
    Stripe.Stretch := True;
    Stripe.SetBounds(X, BoxTop, ScaleX(5), Frame.Height);

    BackupBtn := TNewButton.Create(Form);
    BackupBtn.Parent := Form;
    BackupBtn.Caption := 'Back up library…';
    BackupBtn.SetBounds(X, Frame.Top + Frame.Height + ScaleY(12), ScaleX(140), ScaleY(25));
    BackupBtn.OnClick := @BackupClick;
    BackupNote := UninstText(Form, '', X + ScaleX(150), BackupBtn.Top, W - ScaleX(150));
    { Up to six lines: the note says which PocketMedia was reached, and when the backup counts. }
    BackupNote.Height := ScaleY(84);

    T := UninstText(Form, 'Type CONFIRM to acknowledge that your media will be deleted',
      X, BackupBtn.Top + ScaleY(90), W);
    ConfirmEdit := TNewEdit.Create(Form);
    ConfirmEdit.Parent := Form;
    ConfirmEdit.SetBounds(X, T.Top + T.Height + ScaleY(6), W, ScaleY(23));
    ConfirmEdit.OnChange := @ConfirmChange;

    ConfirmButton := TNewButton.Create(Form);
    ConfirmButton.Parent := Form;
    ConfirmButton.Caption := 'Uninstall and delete media';
    ConfirmButton.SetBounds(X + W - ScaleX(180), ConfirmEdit.Top + ConfirmEdit.Height + ScaleY(16), ScaleX(180), ScaleY(25));
    ConfirmButton.ModalResult := mrOk;
    ConfirmButton.Enabled := False;

    CancelBtn := TNewButton.Create(Form);
    CancelBtn.Parent := Form;
    CancelBtn.Caption := 'Cancel';
    CancelBtn.SetBounds(ConfirmButton.Left - ScaleX(88), ConfirmButton.Top, ScaleX(80), ScaleY(25));
    CancelBtn.ModalResult := mrCancel;
    CancelBtn.Cancel := True;

    Form.ActiveControl := ConfirmEdit;
    Res := Form.ShowModal;
    { Checked again here, not only through the button state. }
    Result := (Res = mrOk) and (ConfirmEdit.Text = 'CONFIRM');
  finally
    Form.Free;
  end;
end;

function InitializeUninstall: Boolean;
begin
  if UninstallSilent then begin
    { A silent uninstall cannot get the confirmation, so it is refused.
      No command-line flag skips the CONFIRM step. }
    Log('Refused: PocketMedia cannot be uninstalled silently, because uninstalling deletes the Media folder ' +
        'and needs CONFIRM typed. Nothing was removed.');
    Result := False;
    Exit;
  end;
  Result := AskForConfirm;
  if Result then
    Log('CONFIRM typed; uninstalling.')
  else
    Log('Uninstall cancelled; nothing was removed.');
end;

{ Lists what is left under Path (not following links), at most a few lines. }
procedure ListLeftovers(const Path: String; var Lines: String; var Count: Integer);
var
  FR: TFindRec;
begin
  if Count >= 15 then
    Exit;
  if FindFirst(AddBackslash(Path) + '*', FR) then begin
    try
      repeat
        if (FR.Name <> '.') and (FR.Name <> '..') then begin
          if (FR.Attributes and FILE_ATTRIBUTE_DIRECTORY <> 0) and
             (FR.Attributes and FILE_ATTRIBUTE_REPARSE_POINT = 0) then
            ListLeftovers(AddBackslash(Path) + FR.Name, Lines, Count)
          else if Count < 15 then begin
            Lines := Lines + AddBackslash(Path) + FR.Name + #13#10;
            Count := Count + 1;
          end;
        end;
      until (Count >= 15) or not FindNext(FR);
    finally
      FindClose(FR);
    end;
  end;
end;

{ Deletes one of PocketMedia's own folders. DelTree removes a junction or
  symbolic link itself and never deletes what it points to. }
procedure RemoveOwnedFolder(const Path: String);
var
  Lines: String;
  Count: Integer;
begin
  if not DirExists(Path) then
    Exit;
  if DelTree(Path, True, True, True) and not DirExists(Path) then
    Log('Removed ' + Path)
  else begin
    Lines := '';
    Count := 0;
    ListLeftovers(Path, Lines, Count);
    if Lines = '' then
      Lines := Path + #13#10;
    RemoveProblems := RemoveProblems + Lines;
    Log('Could not remove everything in ' + Path + ':' + #13#10 + Lines);
  end;
end;

procedure RemoveAutostart(const App: String);
var
  Stored: String;
begin
  { Only the value that starts this copy; another program's or another
    install's entry is left alone. }
  if RegQueryStringValue(HKEY_CURRENT_USER, RunKey, RunValue, Stored) and
     (CompareText(Stored, AutostartCommand(App)) = 0) then begin
    if RegDeleteValue(HKEY_CURRENT_USER, RunKey, RunValue) then
      Log('Removed the Autostart entry.')
    else
      RemoveProblems := RemoveProblems + 'The Autostart entry (HKCU\' + RunKey + '\' + RunValue + ')' + #13#10;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  App: String;
begin
  App := RemoveBackslashUnlessRoot(ExpandConstant('{app}'));
  if CurUninstallStep = usUninstall then begin
    { Before Inno removes its own files, so it can also remove the install
      folder when nothing else is in it. Unrelated files there are kept. }
    RemoveProblems := '';
    RemoveOwnedFolder(AddBackslash(App) + 'Media');
    RemoveOwnedFolder(AddBackslash(App) + 'Data');
    RemoveOwnedFolder(AddBackslash(App) + 'Cache');
    RemoveAutostart(App);
  end
  else if (CurUninstallStep = usPostUninstall) and (RemoveProblems <> '') then
    MsgBox('PocketMedia was uninstalled, but these could not be removed:' + #13#10 + #13#10 +
      RemoveProblems + #13#10 + 'Close any program using them, then delete them yourself.', mbError, MB_OK);
end;
