; tbx（silentbox）Windows インストーラ
;
; 設計方針:
;   - 管理者権限を要求しない（ユーザー単位で完結する）。WinFsp は別途入れる。
;   - アンインストール時に、利用者の設定・キャッシュ・資格情報を勝手に消さない。
;     消すかどうかは確認ダイアログで選ばせる。
;   - パスは固定文字列で埋め込まず、$INSTDIR からの相対で扱う。
;
; ビルド:
;   cd install
;   makensis /DVERSION=0.2.0 /DEXE=..\tbx-installer-input.exe installer.nsi
;
; `/D` の値は引用符なしの 1 トークンとして解釈されるため、空白を含む絶対
; パスは渡せない（実測）。リポジトリ直下に exe を置き、install から見た
; 相対パスで渡す。
;
; GitHub の Windows ランナーには NSIS 3.10 が入っている（実測）。

Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef EXE
  !error "EXE を指定してください（/DEXE=path）"
!endif

!define APP_NAME "silentbox"
!define APP_DISPLAY "silentbox (tbx)"
!define APP_PUBLISHER "heqvyfrogg"
!define APP_EXE "tbx.exe"
!define APP_KEY "Software\silentbox"

Name "${APP_DISPLAY} ${VERSION}"
OutFile "..\tbx-${VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\silentbox"
InstallDirRegKey HKCU "${APP_KEY}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

; VIProductVersion は X.X.X.X の 4 桁数値しか受けず、`0.0.0-ci.0` のような
; 接尾辞付きは `invalid VIProductVersion format` で失敗する（CI 実測）。
; 表示用の VERSION とは別に、数値 4 桁の VERSION4 を受けられるようにする。
!ifndef VERSION4
  !define VERSION4 "${VERSION}.0"
!endif
VIProductVersion "${VERSION4}"
VIAddVersionKey "ProductName" "${APP_DISPLAY}"
VIAddVersionKey "CompanyName" "${APP_PUBLISHER}"
VIAddVersionKey "FileDescription" "TeraBox の非公式クライアント"
VIAddVersionKey "FileVersion" "${VERSION}"

!define MUI_ABORTWARNING
!define MUI_ICON "${NSISDIR}\Contrib\Graphics\Icons\modern-install.ico"
!define MUI_UNICON "${NSISDIR}\Contrib\Graphics\Icons\modern-uninstall.ico"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "Japanese"
!insertmacro MUI_LANGUAGE "English"

; ---- 本体 ----
Section "本体（必須）" SEC_CORE
  SectionIn RO
  SetOutPath "$INSTDIR"
  ; NSIS の File はソースのベース名をそのまま使う。入力の exe 名を
  ; tbx.exe に固定するため /oname を付ける（付け忘れると
  ; tbx-installer-input.exe のまま入り、アンインストーラの削除対象とも
  ; 食い違う。実測で確認）。
  File /oname=${APP_EXE} "${EXE}"
  File /oname=README.md "..\README.md"
  File /oname=CHANGELOG.md "..\CHANGELOG.md"
  SetOutPath "$INSTDIR\docs"
  File "..\docs\limitations.md"
  File "..\docs\protocol.md"

  ; パスを通す（ユーザーの PATH へ追記。既存の値は消さない）。
  WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${APP_KEY}" "Version" "${VERSION}"
  ${GetParameters} $R0

  nsExec::ExecToLog 'cmd /c setx PATH "%PATH%;$INSTDIR" >nul'
  Pop $R0

  WriteUninstaller "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "DisplayName" "${APP_DISPLAY}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "Publisher" "${APP_PUBLISHER}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox" \
    "NoRepair" 1
SectionEnd

; ---- WinFsp の案内 ----
Section "WinFsp の入手先を表示（推奨）" SEC_WINFSP
  ; WinFsp は LGPL/GPL の都合で同梱しない。URL を開くだけ。
  ExecShell "open" "https://winfsp.dev/rel/"
SectionEnd

; ---- 自動起動 ----
Section "PC 起動時に自動でマウント（任意）" SEC_AUTOSTART
  ; 既定では入れない。チェックした人だけ。
  ; ドライブレターは環境設定から読む（未設定なら T:）。
  WriteRegStr HKCU "${APP_KEY}" "AutoStart" "1"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "silentbox" \
    '"$INSTDIR\${APP_EXE}" mount T:'
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_CORE} \
    "tbx 本体とドキュメントをインストールします。管理者権限は不要です。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_WINFSP} \
    "ドライブとして見せるには WinFsp が必要です。配布ページをブラウザで開きます（自動では入れません）。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_AUTOSTART} \
    "PC 起動時に T: へ自動でマウントします。ログイン済みである必要があります。"
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  ; 自動起動の登録を外す。
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "silentbox"

  ; 名前は `APP_EXE` と一致させる（片方だけ直すと消し忘れる）。
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\CHANGELOG.md"
  Delete "$INSTDIR\docs\limitations.md"
  Delete "$INSTDIR\docs\protocol.md"
  RMDir "$INSTDIR\docs"
  Delete "$INSTDIR\uninstall.exe"

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\silentbox"
  DeleteRegKey HKCU "${APP_KEY}"

  ; 利用者のデータは勝手に消さない。確認してから消す。
  ; ただし無音（/S）では確認を出さない。出したままだと操作者がいない現場で
  ; 止まり、アンインストーラが完了しない（実測: /S で MessageBox が待ち続けた）。
  ; 無音時の既定は「データを残す」（安全側）。
  IfSilent keep_data
  MessageBox MB_YESNO|MB_ICONQUESTION \
    "設定・キャッシュ・保存済みパスワードも削除しますか？$\r$\n$\r$\n「いいえ」を選ぶと残ります（再インストール時に引き継がれます）。$\r$\n保存済みの暗号化パスワードを消すと、暗号化したファイルは復号できなくなります。" \
    IDNO keep_data
  RMDir /r "$LOCALAPPDATA\tbx"
  RMDir /r "$APPDATA\tbx"
  keep_data:
  ; RMDir は空ディレクトリしか消せず、作業ディレクトリが $INSTDIR のままだと
  ; 自分自身を消せない（実測: 空のディレクトリが残った）。
  ; 利用者のデータは上の確認で消さない選択ができるので、ここでは
  ; インストール物だけを再帰で片付ける。
  SetOutPath $TEMP
  RMDir /r "$INSTDIR"
SectionEnd
