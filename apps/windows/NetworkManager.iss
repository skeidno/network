#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\NetworkManager"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\release-assets"
#endif

#define MyAppName "Network Manager"
#define MyAppExeName "NetworkManager.exe"

[Setup]
AppId={{D8D55D86-925B-4B5D-BEBB-F2DE2AF601A1}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher=skeidno
AppPublisherURL=https://github.com/skeidno/network
AppSupportURL=https://github.com/skeidno/network/issues
AppUpdatesURL=https://github.com/skeidno/network/releases
DefaultDirName={localappdata}\Programs\Network Manager
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir={#OutputDir}
OutputBaseFilename=NetworkManager-Setup-x64-v{#MyAppVersion}
SetupIconFile=..\..\src\network_manager\web\icons\network-manager.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
UsePreviousAppDir=yes
VersionInfoVersion={#MyAppVersion}.0
VersionInfoCompany=skeidno
VersionInfoDescription=Network Manager Installer
VersionInfoProductName={#MyAppName}
VersionInfoProductVersion={#MyAppVersion}

[Languages]
; 中文语言文件随仓库提供（apps\windows\languages\）。不能再用
; compiler:Languages\ChineseSimplified.isl —— CI 上的 Inno Setup 并不带这个
; 语言包，编译会直接中止。文件必须带 UTF-8 BOM，否则会被按 ANSI 读成乱码。
Name: "chinesesimp"; MessagesFile: "languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce

[Files]
; 主程序、mihomo 内核、wintun.dll（及其 LICENSE）整个目录复制到 {app}。
; 排除在线安装器：它只在真需要时才解压到 {tmp}，不该落到安装目录里 ——
; 留着只会让用户以为那是个要手动点的东西。
Source: "{#SourceDir}\*"; DestDir: "{app}"; Excludes: "MicrosoftEdgeWebview2Setup.exe"; Flags: ignoreversion recursesubdirs createallsubdirs
#ifdef HasWebView2Bootstrapper
; dontcopy：不在安装一开始就解压，只有 [Code] 里显式 ExtractTemporaryFile 时才落到
; {tmp}，安装结束自动删除。见 PrepareToInstall。
Source: "{#SourceDir}\MicrosoftEdgeWebview2Setup.exe"; DestDir: "{tmp}"; Flags: dontcopy
#endif

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"; Tasks: desktopicon

[Registry]
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "NetWorkManger"; Flags: uninsdeletevalue

[Run]
; WebView2 必须在程序第一次启动前就位，否则窗口是空白的。这条不带 postinstall，
; 因此它在复制文件阶段就执行完毕，不会混进「安装完成」页的勾选项里。
; /silent /install 是 Evergreen Bootstrapper 的无交互安装参数（首次需要联网下载运行时）。
; Check 为假时整条跳过 —— 已装 WebView2 的机器上这一步完全不存在。
#ifdef HasWebView2Bootstrapper
Filename: "{tmp}\MicrosoftEdgeWebview2Setup.exe"; Parameters: "/silent /install"; StatusMsg: "正在安装 WebView2 运行时（首次需联网下载，请稍候）…"; Check: NeedWebView2Runtime; Flags: waituntilterminated runhidden
#endif
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent shellexec

[Code]
const
  // 微软官方给出的 WebView2 检测位置。三处都要查：64 位系统上运行时通常落在
  // 32 位注册表视图（WOW6432Node）；也可能存在按用户安装的情形（HKCU）。
  WebView2ClientKey = 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';

function QueryWebView2Version(RootKey: Integer; var Version: String): Boolean;
begin
  // pv 为空或 '0.0.0.0' 都算「没有可用的运行时」：卸载后注册表键可能残留下来。
  Result := RegQueryStringValue(RootKey, WebView2ClientKey, 'pv', Version)
    and (Version <> '') and (Version <> '0.0.0.0');
end;

function WebView2RuntimeInstalled(): Boolean;
var
  Version: String;
begin
  Result := QueryWebView2Version(HKLM32, Version)
    or QueryWebView2Version(HKLM, Version)
    or QueryWebView2Version(HKCU, Version);
end;

function NeedWebView2Runtime(): Boolean;
begin
  Result := not WebView2RuntimeInstalled();
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if WebView2RuntimeInstalled() then
    Exit;

#ifdef HasWebView2Bootstrapper
  // 运行时缺失：把内置的在线安装器解压到 {tmp}，交给上面 [Run] 段静默安装。
  ExtractTemporaryFile('MicrosoftEdgeWebview2Setup.exe');
#else
  // 没有内置安装器时只能把话说清楚 —— 界面依赖 WebView2，装完不补上就是空白窗口。
  if MsgBox('未检测到 Microsoft Edge WebView2 运行时，程序界面依赖它才能显示。' + #13#10 + #13#10 +
            '本安装包未内置在线安装器（打包时下载失败）。请安装完成后手动安装：' + #13#10 +
            'https://developer.microsoft.com/microsoft-edge/webview2/' + #13#10 + #13#10 +
            '仍然继续安装吗？', mbConfirmation, MB_YESNO) = IDNO then
    Result := '已取消：缺少 WebView2 运行时。';
#endif
end;
