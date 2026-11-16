;  ____                 ____
; | __ )  _   _  _   _ |  _ \   ___ __   __
; |  _ \ | | | || | | || | | | / _ \\ \ / /
; | |_) || |_| || |_| || |_| ||  __/ \ V /
; |____/  \__,_| \__,_||____/  \___|  \_/
;
; BTMS · Buu's Tiles Map Scanner
; Author : BuuDevOff · https://github.com/BuuDevOff
; SPDX-License-Identifier: GPL-3.0-only
;
; The Windows installer (Inno Setup 6). Built by .github/workflows/release.yml after
; `cargo build --release`:
;
;   iscc /DAppVersion=1.2.3 /DAppNumericVersion=1.2.3 installer\btms.iss
;
; It installs for the current user by default (no administrator rights, in
; %LOCALAPPDATA%\Programs\BTMS), or for all users when chosen in the first dialog. The working
; folder is Documents\BTMS: the shortcut starts BTMS there, map.toml is copied there once (never
; overwritten, never removed), and renders go to its output folder by default.

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif
; the version without a pre-release or build suffix (1.2.3 of 1.2.3-beta.1): Windows wants numbers only
#ifndef AppNumericVersion
  #define AppNumericVersion AppVersion
#endif

#define AppName "BTMS"
#define AppFullName "BTMS · Buu's Tiles Map Scanner"
#define AppPublisher "BuuDevOff"
#define AppURL "https://github.com/BuuDevOff/BTMS"
; btms.exe is the command line (and the PATH entry); btms-gui.exe is the same program built as a
; Windows application, so the shortcuts open the interface without a console window
#define AppExe "btms.exe"
#define AppGui "btms-gui.exe"
; the folder of the release build (`/DBinDir=…` for another target directory)
#ifndef BinDir
  #define BinDir "..\target\release"
#endif
; the official file of CodeWalker (github.com/dexyfex/CodeWalker): BTMS never ships it, the setup
; downloads it only when the keys task is ticked
#define MagicUrl "https://raw.githubusercontent.com/dexyfex/CodeWalker/master/CodeWalker.Core/Resources/magic.dat"

[Setup]
; never change the AppId: Windows recognises installs and upgrades of BTMS by it
AppId={{183E6B20-D181-4CCE-9471-1BABE8380D1D}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppFullName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
AppCopyright=Copyright © {#AppPublisher}. Licensed under GPL-3.0-only.
VersionInfoVersion={#AppNumericVersion}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppFullName} installer
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\LICENSE
SetupIconFile=..\assets\icon\btms.ico
UninstallDisplayIcon={app}\{#AppGui}
UninstallDisplayName={#AppFullName}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
ChangesEnvironment=yes
ShowLanguageDialog=auto
OutputDir=..\dist
OutputBaseFilename=BTMS-Setup-{#AppVersion}-x64

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"
Name: "de"; MessagesFile: "compiler:Languages\German.isl"
Name: "nl"; MessagesFile: "compiler:Languages\Dutch.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "sv"; MessagesFile: "compiler:Languages\Swedish.isl"
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"

[CustomMessages]
en.AddToPath=Add btms to the PATH (to run it from any terminal)
fr.AddToPath=Ajouter btms au PATH (pour le lancer depuis n'importe quel terminal)
de.AddToPath=btms zum PATH hinzufügen (um es in jedem Terminal zu starten)
nl.AddToPath=btms aan het PATH toevoegen (om het vanuit elke terminal te starten)
ru.AddToPath=Добавить btms в PATH (чтобы запускать его из любого терминала)
sv.AddToPath=Lägg till btms i PATH (för att köra det från valfri terminal)
es.AddToPath=Añadir btms al PATH (para ejecutarlo desde cualquier terminal)
en.CommandLine=Command line:
fr.CommandLine=Ligne de commande :
de.CommandLine=Befehlszeile:
nl.CommandLine=Opdrachtregel:
ru.CommandLine=Командная строка:
sv.CommandLine=Kommandorad:
es.CommandLine=Línea de comandos:
en.AppComment=Export the GTA V map as web map tiles
fr.AppComment=Exporter la carte de GTA V en tuiles web
de.AppComment=Die GTA-V-Karte als Web-Kartenkacheln exportieren
nl.AppComment=De GTA V-kaart exporteren als webkaarttegels
ru.AppComment=Экспорт карты GTA V в веб-тайлы
sv.AppComment=Exportera GTA V-kartan som webbkartplattor
es.AppComment=Exportar el mapa de GTA V en teselas web
en.KeysGroup=GTA V decryption keys:
fr.KeysGroup=Clés de déchiffrement de GTA V :
de.KeysGroup=Entschlüsselungsschlüssel von GTA V:
nl.KeysGroup=Ontsleutelingssleutels van GTA V:
ru.KeysGroup=Ключи расшифровки GTA V:
sv.KeysGroup=Avkrypteringsnycklar för GTA V:
es.KeysGroup=Claves de descifrado de GTA V:
en.ImportKeys=Set up the keys now: download magic.dat from CodeWalker's official GitHub
fr.ImportKeys=Installer les clés maintenant : télécharger magic.dat depuis le GitHub officiel de CodeWalker
de.ImportKeys=Schlüssel jetzt einrichten: magic.dat vom offiziellen GitHub von CodeWalker herunterladen
nl.ImportKeys=Sleutels nu instellen: magic.dat downloaden van de officiële GitHub van CodeWalker
ru.ImportKeys=Установить ключи сейчас: загрузить magic.dat с официального GitHub CodeWalker
sv.ImportKeys=Konfigurera nycklarna nu: ladda ned magic.dat från CodeWalkers officiella GitHub
es.ImportKeys=Configurar las claves ahora: descargar magic.dat del GitHub oficial de CodeWalker
en.DownloadingKeys=Downloading magic.dat from CodeWalker's official GitHub (github.com/dexyfex/CodeWalker).
fr.DownloadingKeys=Téléchargement de magic.dat depuis le GitHub officiel de CodeWalker (github.com/dexyfex/CodeWalker).
de.DownloadingKeys=magic.dat wird vom offiziellen GitHub von CodeWalker heruntergeladen (github.com/dexyfex/CodeWalker).
nl.DownloadingKeys=magic.dat wordt gedownload van de officiële GitHub van CodeWalker (github.com/dexyfex/CodeWalker).
ru.DownloadingKeys=Загрузка magic.dat с официального GitHub CodeWalker (github.com/dexyfex/CodeWalker).
sv.DownloadingKeys=magic.dat laddas ned från CodeWalkers officiella GitHub (github.com/dexyfex/CodeWalker).
es.DownloadingKeys=Descargando magic.dat del GitHub oficial de CodeWalker (github.com/dexyfex/CodeWalker).
en.ImportingKeys=Setting up the decryption keys...
fr.ImportingKeys=Installation des clés de déchiffrement...
de.ImportingKeys=Entschlüsselungsschlüssel werden eingerichtet...
nl.ImportingKeys=Ontsleutelingssleutels worden ingesteld...
ru.ImportingKeys=Установка ключей расшифровки...
sv.ImportingKeys=Avkrypteringsnycklarna konfigureras...
es.ImportingKeys=Configurando las claves de descifrado...
en.KeysFailed=The decryption keys could not be set up automatically: no network, or GTA V was not found through FiveM. BTMS is installed all the same. Import magic.dat later from the BTMS window, or with the command: btms import-keys <magic.dat> --game <GTA V folder>
fr.KeysFailed=Les clés de déchiffrement n'ont pas pu être installées automatiquement : pas de réseau, ou GTA V introuvable via FiveM. BTMS est tout de même installé. Importez magic.dat plus tard depuis la fenêtre de BTMS, ou avec la commande : btms import-keys <magic.dat> --game <dossier de GTA V>
de.KeysFailed=Die Entschlüsselungsschlüssel konnten nicht automatisch eingerichtet werden: kein Netzwerk, oder GTA V wurde über FiveM nicht gefunden. BTMS ist trotzdem installiert. Importieren Sie magic.dat später im BTMS-Fenster oder mit dem Befehl: btms import-keys <magic.dat> --game <GTA-V-Ordner>
nl.KeysFailed=De ontsleutelingssleutels konden niet automatisch worden ingesteld: geen netwerk, of GTA V werd niet via FiveM gevonden. BTMS is toch geïnstalleerd. Importeer magic.dat later vanuit het BTMS-venster, of met de opdracht: btms import-keys <magic.dat> --game <GTA V-map>
ru.KeysFailed=Не удалось автоматически установить ключи расшифровки: нет сети, или GTA V не найдена через FiveM. BTMS всё равно установлен. Импортируйте magic.dat позже в окне BTMS или командой: btms import-keys <magic.dat> --game <папка GTA V>
sv.KeysFailed=Avkrypteringsnycklarna kunde inte konfigureras automatiskt: inget nätverk, eller så hittades inte GTA V via FiveM. BTMS är ändå installerat. Importera magic.dat senare i BTMS-fönstret, eller med kommandot: btms import-keys <magic.dat> --game <GTA V-mapp>
es.KeysFailed=No se pudieron configurar automáticamente las claves de descifrado: no hay red, o no se encontró GTA V a través de FiveM. BTMS se ha instalado de todos modos. Importe magic.dat más tarde desde la ventana de BTMS, o con el comando: btms import-keys <magic.dat> --game <carpeta de GTA V>

[Tasks]
; magic.dat is never shipped with BTMS: this task downloads it from CodeWalker's official repository,
; only when the box is ticked, then `btms import-keys` decodes and checks the keys
Name: "importkeys"; Description: "{cm:ImportKeys}"; GroupDescription: "{cm:KeysGroup}"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "{cm:AddToPath}"; GroupDescription: "{cm:CommandLine}"; Flags: unchecked

[Dirs]
Name: "{userdocs}\{#AppName}"; Flags: uninsneveruninstall

[Files]
Source: "{#BinDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\{#AppGui}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\map.toml"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\CHANGELOG.md"; DestDir: "{app}"; Flags: ignoreversion
; the user's own copy of the configuration: written once, kept on upgrade and on uninstall
Source: "..\map.toml"; DestDir: "{userdocs}\{#AppName}"; Flags: onlyifdoesntexist uninsneveruninstall

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppGui}"; WorkingDir: "{userdocs}\{#AppName}"; Comment: "{cm:AppComment}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppGui}"; WorkingDir: "{userdocs}\{#AppName}"; Comment: "{cm:AppComment}"; Tasks: desktopicon

[Registry]
Root: HKA; Subkey: "{code:EnvironmentKey}"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\{#AppGui}"; WorkingDir: "{userdocs}\{#AppName}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent runasoriginaluser

[Code]
// The environment key of the install mode: the machine's for all users, the user's otherwise.
function EnvironmentKey(Param: String): String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

// Whether Dir is missing from the Path of the install mode (compared without case).
function NeedsAddPath(Dir: String): Boolean;
var
  Paths: String;
begin
  if not RegQueryStringValue(HKA, EnvironmentKey(''), 'Path', Paths) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;

var
  DownloadPage: TDownloadWizardPage;

function OnDownloadProgress(const Url, FileName: String; const Progress, ProgressMax: Int64): Boolean;
begin
  Result := True;
end;

procedure InitializeWizard;
begin
  DownloadPage := CreateDownloadPage(SetupMessage(msgWizardPreparing), CustomMessage('DownloadingKeys'), @OnDownloadProgress);
end;

// The keys task, step 1, when Install is clicked: magic.dat is downloaded into the setup's temporary
// folder. A failure (no network, download cancelled) never stops the install: step 2 says so.
function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if (CurPageID <> wpReady) or not WizardIsTaskSelected('importkeys') then
    exit;
  DownloadPage.Clear;
  DownloadPage.Add('{#MagicUrl}', 'magic.dat', '');
  DownloadPage.Show;
  try
    try
      DownloadPage.Download;
    except
      Log('magic.dat was not downloaded: ' + GetExceptionMessage);
    end;
  finally
    DownloadPage.Hide;
  end;
end;

// The keys task, step 2, once the files are in place: `btms import-keys` decodes magic.dat with the
// AES key of the user's GTA5.exe (found through FiveM) and checks every key against its SHA-1. Run
// as the user who started the setup, so the keys go to their %LOCALAPPDATA%\BTMS\keys even in an
// all-users install. Only the verified keys are kept; magic.dat goes with the temporary folder.
procedure CurStepChanged(CurStep: TSetupStep);
var
  Magic: String;
  ResultCode: Integer;
begin
  if (CurStep <> ssPostInstall) or not WizardIsTaskSelected('importkeys') then
    exit;
  Magic := ExpandConstant('{tmp}\magic.dat');
  WizardForm.StatusLabel.Caption := CustomMessage('ImportingKeys');
  if not FileExists(Magic)
    or not ExecAsOriginalUser(ExpandConstant('{app}\{#AppExe}'), 'import-keys "' + Magic + '"', ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, ResultCode)
    or (ResultCode <> 0) then
    SuppressibleMsgBox(CustomMessage('KeysFailed'), mbInformation, MB_OK, IDOK);
end;

// On uninstall, takes the install folder out of the Path again (when the task had put it there).
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Paths, Dir: String;
  P: Integer;
begin
  if CurUninstallStep <> usPostUninstall then
    exit;
  if not RegQueryStringValue(HKA, EnvironmentKey(''), 'Path', Paths) then
    exit;
  Dir := ExpandConstant('{app}');
  // P is where ';' + Dir + ';' starts in ';' + Paths + ';', so Dir starts at P in Paths
  P := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';');
  if P = 0 then
    exit;
  if P > 1 then
    Delete(Paths, P - 1, Length(Dir) + 1)
  else
    Delete(Paths, P, Length(Dir) + 1);
  RegWriteExpandStringValue(HKA, EnvironmentKey(''), 'Path', Paths);
end;
