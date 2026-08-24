; L'installeur Windows : Claudhub-Setup-x86_64.exe.
;
; Il ne remplace pas le .exe nu, qui reste publié à côté — un seul fichier,
; rien à installer, c'est ce que promet le README. Ce que l'installeur ajoute
; est ce qu'un fichier posé dans les téléchargements ne peut pas avoir : une
; icône sur le bureau, une entrée dans le menu Démarrer, « Ouvrir avec
; Claudhub » sur un dossier, et de quoi désinstaller.
;
; **Sans droits d'administrateur** (`PrivilegesRequired=lowest`) : Claudhub
; n'écrit rien hors du profil de l'utilisateur, ne pose aucun service et
; n'enregistre rien pour la machine entière. Demander l'élévation ne servirait
; qu'à faire apparaître une invite UAC, et à mettre dans Program Files un
; exécutable que la mise à jour suivante ne pourrait plus remplacer sans elle.
; `{autopf}` vaut donc `%LOCALAPPDATA%\Programs`, et `HKA` vaut `HKCU`.
;
; Compilé par la jambe Windows de la CI, après le build release :
;   ISCC.exe tools\claudhub.iss
; Le répertoire courant n'a pas d'importance : les chemins des sections se
; résolvent depuis le dossier du script, et celui de l'exécutable est rendu
; absolu plus bas. `/DSourceExe=…` en nomme un autre.

; Absolu, et par `SourcePath` : les chemins des sections se résolvent depuis le
; dossier du script, mais ceux que reçoit une **fonction du préprocesseur** —
; `GetVersionNumbersString` ci-dessous — se résolvent depuis le répertoire
; courant d'ISCC. Un `..\target\…` relatif marcherait dans `[Files]` et
; nommerait le parent du dépôt dans la ligne d'après.
#ifndef SourceExe
  #define SourceExe SourcePath + "\..\target\release\claudhub.exe"
#endif

; La version vient de l'exécutable lui-même, où `build.rs` a écrit celle de
; `Cargo.toml` : deux listes divergeraient au premier `just release`, et c'est
; cette version que Windows affiche dans « Applications installées ».
#define AppVersion GetVersionNumbersString(SourceExe)
#define AppName "Claudhub"
#define AppExe "Claudhub.exe"
#define AppUrl "https://github.com/Catvert/Claudhub"

[Setup]
; Cet identifiant est ce par quoi Windows reconnaît une installation déjà là :
; il ne change jamais, sous peine de laisser deux Claudhub dans la liste des
; applications, dont un que plus rien ne désinstalle.
AppId={{A17134D9-C861-40A1-B2FE-C6857246DB36}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Catvert
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
DefaultDirName={autopf}\{#AppName}
; La question du dossier du menu Démarrer pour un seul raccourci : du bruit.
; Le raccourci va d'ailleurs directement dans `{autoprograms}`, sans dossier —
; rien n'emploie `{group}`.
DisableProgramGroupPage=yes
; `x64` et non `x64compatible` : le nom moderne demande Inno 6.3, et la version
; installée sur le runner n'est pas de nous. L'ancien reste accepté.
; Restreint à x64 délibérément — le serveur embarqué est un binaire musl x86_64,
; et une machine ARM ne saurait pas quoi en faire dans sa distribution.
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
PrivilegesRequired=lowest
OutputDir=..\target\setup
OutputBaseFilename=Claudhub-Setup-x86_64
SetupIconFile=..\assets\claudhub.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/max
WizardStyle=modern
; Une mise à jour par-dessus une instance qui tourne échouerait sur un fichier
; verrouillé, et l'installeur redémarrerait Windows pour s'en sortir. Il
; propose donc de la fermer, et rien ne redémarre.
CloseApplications=yes
RestartApplications=no
RestartIfNeededByRun=no
LicenseFile=..\LICENSE

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"

[CustomMessages]
; Seulement ce qu'Inno ne traduit pas déjà : `CreateDesktopIcon`,
; `AdditionalIcons` et `LaunchProgram` sont dans les deux catalogues livrés,
; et les redéfinir serait deux traductions à tenir au lieu de zéro.
en.ExplorerContext=Add "Open with Claudhub" to a folder's context menu
en.OpenWith=Open with Claudhub
fr.ExplorerContext=Ajouter « Ouvrir avec Claudhub » au menu contextuel d'un dossier
fr.OpenWith=Ouvrir avec Claudhub

[Tasks]
; Cochée : c'est le geste pour lequel on prend un installeur plutôt que le
; .exe nu.
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "explorercontext"; Description: "{cm:ExplorerContext}"

[Files]
; Renommé : les raccourcis, le gestionnaire des tâches et la barre des tâches
; montrent ce nom-là. Le fichier publié à côté garde le sien, qui dit
; l'architecture — on le télécharge, on ne le lit pas dans une liste de
; processus.
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; « Ouvrir avec Claudhub » sur un dossier, et dans le fond d'un dossier ouvert
; — ce sont deux clés distinctes, et n'en poser qu'une donne un menu qui
; apparaît sur l'icône et pas dans la fenêtre.
;
; `%V` et non `%1` : dans le fond d'un dossier, `%1` ne nomme rien. Et c'est un
; **argument**, parce que le répertoire courant d'un verbe est celui que
; l'explorateur avait, pas celui sur lequel on a cliqué (voir
; `instance::launch_argument`).
;
; Le processus lancé ici ne fait pas forcément une fenêtre : si un Claudhub
; tourne déjà, il lui passe le dossier par une socket locale et s'arrête
; (`src/instance.rs`).
Root: HKA; Subkey: "Software\Classes\Directory\shell\Claudhub"; ValueType: string; ValueData: "{cm:OpenWith}"; Flags: uninsdeletekey; Tasks: explorercontext
Root: HKA; Subkey: "Software\Classes\Directory\shell\Claudhub"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"; Tasks: explorercontext
Root: HKA; Subkey: "Software\Classes\Directory\shell\Claudhub\command"; ValueType: string; ValueData: """{app}\{#AppExe}"" ""%V"""; Tasks: explorercontext
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Claudhub"; ValueType: string; ValueData: "{cm:OpenWith}"; Flags: uninsdeletekey; Tasks: explorercontext
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Claudhub"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"; Tasks: explorercontext
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Claudhub\command"; ValueType: string; ValueData: """{app}\{#AppExe}"" ""%V"""; Tasks: explorercontext
; Et l'inverse : décocher la case en réinstallant doit retirer l'entrée. Inno
; ne défait pas ce qu'une tâche a posé quand elle cesse d'être choisie, si bien
; que le menu resterait là sans qu'aucune case ne le dise.
Root: HKA; Subkey: "Software\Classes\Directory\shell\Claudhub"; ValueType: none; Flags: deletekey; Tasks: not explorercontext
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Claudhub"; ValueType: none; Flags: deletekey; Tasks: not explorercontext

[Run]
; `nowait` : l'installeur rendrait la main à la fermeture de l'application,
; laissant sa fenêtre de fin ouverte pendant toute une session de travail.
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
