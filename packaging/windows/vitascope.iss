; 影戲 VitaScope — Windows 安裝程式（Inno Setup 6.3 以上）
;
; 發佈流程的用法（版本號與來源資料夾由命令列傳入）：
;   iscc /Qp /DAppVersion=0.2.0 /DSourceDir=C:\...\dist\VitaScope-v0.2.0-windows-x64 ^
;        /OC:\...\dist /FVitaScope-v0.2.0-windows-x64-setup packaging\windows\vitascope.iss
;
; 每位使用者各自安裝（不需要系統管理員）：%LOCALAPPDATA%\Programs\VitaScope
; 設定與播放紀錄在 %APPDATA%\Vitascope（解除安裝時保留），暫存在 %LOCALAPPDATA%\VitaScope（解除安裝時刪除）
; 檔案關聯寫的登錄機碼跟程式「設定 → 系統」寫的一樣（src/assoc.rs），兩邊可以互相移除

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #error 請用 /DSourceDir=... 指定已打包好的 Windows 資料夾（含 vitascope.exe 與 libmpv-2.dll）
#endif

; 檔案版本只能是數字（0.2.0-dev → 0.2.0）
#if Pos("-", AppVersion) > 0
  #define NumVersion Copy(AppVersion, 1, Pos("-", AppVersion) - 1)
#else
  #define NumVersion AppVersion
#endif

#define AppGuid     "25BEAAEA-93FA-4E38-A6D3-9AE066CD282B"
#define AppName     "影戲 VitaScope"
#define ExeName     "vitascope.exe"
#define ProgIdVideo "VitaScope.Video"
#define ProgIdAudio "VitaScope.Audio"

[Setup]
; AppId 一旦發佈就不能再改：升級、解除安裝都靠它找到舊版
AppId={{{#AppGuid}}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=acer1204
AppPublisherURL=https://github.com/acer1204/VitaScope
AppSupportURL=https://github.com/acer1204/VitaScope/issues
AppUpdatesURL=https://github.com/acer1204/VitaScope/releases
AppCopyright=GPL-3.0-or-later
VersionInfoVersion={#NumVersion}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppName} Setup
; 不需要系統管理員：{autopf} = %LOCALAPPDATA%\Programs，HKA = HKCU
PrivilegesRequired=lowest
DefaultDirName={autopf}\VitaScope
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; libmpv-2.dll 用到 Windows 10 1607 才有的 API（AdjustWindowRectExForDpi、GetSystemMetricsForDpi）
MinVersion=10.0.14393
; 安裝 / 解除安裝結束時通知檔案總管重新整理關聯與圖示
ChangesAssociations=yes
; 影戲開著時先請使用者關掉（程式啟動時建立這個 mutex，見 src/main.rs）；
; 不然解除安裝刪不掉執行檔，開著的視窗關閉時還會把設定寫回去
AppMutex=VitaScope.Running
UninstallDisplayIcon={app}\{#ExeName}
UninstallDisplayName={#AppName}
; 只有在找不到符合 Windows 顯示語言的翻譯時才問語言（預設 yes 會每次都問）
ShowLanguageDialog=auto
WizardStyle=modern
SetupIconFile=..\icons\vitascope.ico
Compression=lzma2/max
SolidCompression=yes
OutputDir=.
OutputBaseFilename=VitaScope-setup

[Languages]
; 依 Windows 顯示語言自動選擇；都不符合時用第一個
Name: "zh_TW"; MessagesFile: "ChineseTraditional.isl"
Name: "en";    MessagesFile: "compiler:Default.isl"

[CustomMessages]
zh_TW.AssocGroup=檔案關聯：
en.AssocGroup=File associations:
zh_TW.AssocTask=把影戲加入影片與音訊檔的「開啟檔案」選單（不會更改預設程式）
en.AssocTask=Add VitaScope to "Open with" for video and audio files (does not change defaults)
zh_TW.VideoFile=影片檔
en.VideoFile=Video file
zh_TW.AudioFile=音訊檔
en.AudioFile=Audio file
zh_TW.AppDescription=以 libmpv 為引擎的影片播放器
en.AppDescription=Video player powered by libmpv

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "assoc"; Description: "{cm:AssocTask}"; GroupDescription: "{cm:AssocGroup}"

[Files]
Source: "{#SourceDir}\{#ExeName}";    DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\libmpv-2.dll";  DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE";       DestDir: "{app}"
Source: "{#SourceDir}\README.md";     DestDir: "{app}"
Source: "{#SourceDir}\THIRD-PARTY-*"; DestDir: "{app}"
; libmpv-2.dll 內含的每個元件的授權條文
Source: "{#SourceDir}\licenses\*";   DestDir: "{app}\licenses"; Flags: recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#ExeName}"
Name: "{autodesktop}\{#AppName}";  Filename: "{app}\{#ExeName}"; Tasks: desktopicon

[Registry]
; 「開啟檔案」清單裡顯示的名稱與指令（不論是否勾選關聯都寫：拖檔到捷徑、「開啟檔案 → 選擇其他應用程式」也用得到）
Root: HKA; Subkey: "Software\Classes\Applications\{#ExeName}"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "{#AppName}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Applications\{#ExeName}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#ExeName},0"
Root: HKA; Subkey: "Software\Classes\Applications\{#ExeName}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#ExeName}"" ""%1"""

; ProgID：影片、音訊各一個。MultiSelectModel=Player：檔案總管選超過 15 個檔案時「開啟」才不會消失
Root: HKA; Subkey: "Software\Classes\{#ProgIdVideo}"; ValueType: string; ValueName: ""; ValueData: "{cm:VideoFile}"; Flags: uninsdeletekey; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\Classes\{#ProgIdVideo}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#ExeName},0"; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\Classes\{#ProgIdVideo}\shell\open"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\Classes\{#ProgIdVideo}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#ExeName}"" ""%1"""; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\Classes\{#ProgIdAudio}"; ValueType: string; ValueName: ""; ValueData: "{cm:AudioFile}"; Flags: uninsdeletekey; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\Classes\{#ProgIdAudio}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#ExeName},0"; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\Classes\{#ProgIdAudio}\shell\open"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\Classes\{#ProgIdAudio}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#ExeName}"" ""%1"""; Tasks: assoc; Check: AssocWanted

; 「設定 → 應用程式 → 預設應用程式」裡列出影戲，使用者可以自己指定（Windows 10/11 不允許程式自行設成預設）
Root: HKA; Subkey: "Software\VitaScope"; Flags: uninsdeletekeyifempty; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\VitaScope\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"; Flags: uninsdeletekey; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\VitaScope\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "{cm:AppDescription}"; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\VitaScope\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: "{app}\{#ExeName},0"; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "VitaScope"; ValueData: "Software\VitaScope\Capabilities"; Flags: uninsdeletevalue; Tasks: assoc; Check: AssocWanted

; 每個副檔名三筆：OpenWithProgids（開啟檔案選單）、SupportedTypes、Capabilities\FileAssociations
; 清單必須與 src/formats.rs 的 VIDEO、AUDIO 相同（VIDEO_RARE 不關聯；測試 assoc::tests 會檢查）
#dim VideoExts[19] {"mp4","m4v","mkv","webm","mov","avi","ts","m2ts","mts","mpg","mpeg","vob","wmv","asf","flv","f4v","3gp","3g2","ogv"}
#dim AudioExts[20] {"mp3","m4a","aac","flac","opus","ogg","oga","wav","wma","ac3","dts","mka","alac","ape","wv","tta","amr","spx","dsf","dff"}
#define Ext ""
#define ProgId ""
#define I 0

#sub AssocExt
Root: HKA; Subkey: "Software\Classes\.{#Ext}\OpenWithProgids"; ValueType: string; ValueName: "{#ProgId}"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc; Check: AssocWanted
Root: HKA; Subkey: "Software\Classes\Applications\{#ExeName}\SupportedTypes"; ValueType: string; ValueName: ".{#Ext}"; ValueData: ""
Root: HKA; Subkey: "Software\VitaScope\Capabilities\FileAssociations"; ValueType: string; ValueName: ".{#Ext}"; ValueData: "{#ProgId}"; Tasks: assoc; Check: AssocWanted
#endsub

; #sub 裡的 #define 是區域變數，AssocExt 看不到；用指定運算式改全域的 Ext / ProgId
#sub VideoOne
  #expr Ext = VideoExts[I], ProgId = ProgIdVideo
  #expr AssocExt
#endsub
#sub AudioOne
  #expr Ext = AudioExts[I], ProgId = ProgIdAudio
  #expr AssocExt
#endsub
#for {I = 0; I < DimOf(VideoExts); I++} VideoOne
#for {I = 0; I < DimOf(AudioExts); I++} AudioOne

[Run]
Filename: "{app}\{#ExeName}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; 安裝後才在程式裡打開的檔案關聯，安裝程式沒有記錄；讓程式自己全部移除（也會關掉設定裡的選項）
Filename: "{app}\{#ExeName}"; Parameters: "--unregister-associations"; Flags: runhidden waituntilterminated; RunOnceId: "UnregisterAssociations"

[InstallDelete]
; 授權條文跟著 libmpv-2.dll 一起換：升級時先清掉，元件拿掉後不會留下舊的條文
Type: filesandordirs; Name: "{app}\licenses"

[UninstallDelete]
; %LOCALAPPDATA%\VitaScope 都是暫存（轉碼後的字幕、翻轉用的著色器、單一執行個體的鎖定檔）
; 與影戲按需求下載的 yt-dlp、deno（tools\）；
; 設定與播放紀錄（%APPDATA%\Vitascope）保留給重新安裝
Type: filesandordirs; Name: "{localappdata}\VitaScope"

[Code]
var
  AssocRemovedByUser: Boolean;

function InitializeSetup(): Boolean;
begin
  // 之前裝過、後來在程式的「設定 → 系統」關掉了檔案關聯：升級時不要再自動加回去
  //（Inno 預設沿用上次安裝時勾的工作）
  AssocRemovedByUser :=
    RegKeyExists(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{{#AppGuid}}_is1') and
    not RegKeyExists(HKCU, 'Software\Classes\{#ProgIdVideo}');
  Result := True;
end;

// 有畫面的安裝：工作頁預設不勾，使用者可以自己再勾
procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpSelectTasks) and AssocRemovedByUser then
  begin
    WizardSelectTasks('!assoc');
    AssocRemovedByUser := False;
  end;
end;

// 無聲安裝（沒有工作頁可以取消）：直接不寫
function AssocWanted(): Boolean;
begin
  Result := not (AssocRemovedByUser and WizardSilent);
end;
